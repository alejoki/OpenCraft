//! Pure item, inventory, and crafting domain.
//!
//! This module has no winit/wgpu dependency so every rule below can be
//! exercised headlessly. Persistence and UI wiring live in other layers.
//!
//! The authoritative model is [`Inventory`]: a fixed nine-slot hotbar plus
//! exactly 27 main slots and an optional cursor, holding validated [`Item`]
//! values. All mutation flows through narrow, bounds-checked operations so no
//! caller can forge, duplicate, or silently lose items.
//!
//! [`ItemStack`] and [`Hotbar`] are the legacy block-only value/collection
//! types kept for existing consumers (mining collection, save format v1-v3,
//! HUD counts). [`Inventory`] exposes read-only compatibility accessors
//! ([`Inventory::hotbar_stack`], [`Inventory::to_legacy_hotbar`]) so those
//! consumers can migrate without regaining mutable slot arrays.

use crate::content::{names, ContentRegistry, RecipeDef};
use crate::world::block::BlockId;
use crate::world::loot::LootItem;
use crate::world::mob_loot::MobDropItem;

/// Number of hotbar slots.
pub const HOTBAR_SLOTS: usize = 9;

/// Number of durable main-inventory slots.
pub const MAIN_SLOTS: usize = 27;

/// Total persisted inventory slots: nine hotbar plus 27 main slots.
pub const INVENTORY_SLOTS: usize = HOTBAR_SLOTS + MAIN_SLOTS;

/// Per-slot cap for block stacks.
///
/// Kept at the historical value so v1-v3 saves containing large block stacks
/// migrate losslessly. Materials use [`MATERIAL_STACK_LIMIT`]; tools are
/// non-stackable.
pub const MAX_STACK_COUNT: u16 = 999;

/// Per-slot cap for non-block materials (e.g. sticks).
pub const MATERIAL_STACK_LIMIT: u16 = 64;

/// Largest storage bound for any crafting grid (4x4 inclusive).
pub const MAX_GRID_SLOTS: usize = 16;

/// Number of durable chest slots in a world chest.
pub const CHEST_SLOTS: usize = 27;

/// Fixed-size chest storage: 27 optional validated items.
pub type ChestSlots = [Option<Item>; CHEST_SLOTS];

/// Side length of the player (inventory) crafting grid.
pub const PLAYER_GRID_DIM: usize = 2;

/// Side length of the crafting-table grid (4x4, as chosen in M006).
pub const TABLE_GRID_DIM: usize = 4;

// ---------------------------------------------------------------------------
// Legacy block-stack compatibility layer
// ---------------------------------------------------------------------------

/// A single legacy hotbar stack: a block id and a finite count.
///
/// New code should use [`Item`]; this type is retained so the existing
/// mining, HUD, and v1-v3 persistence paths keep compiling and round-tripping.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ItemStack {
    pub block: BlockId,
    pub count: u16,
}

impl ItemStack {
    /// Builds a stack, rejecting blocks that cannot be carried and counts
    /// outside `1..=MAX_STACK_COUNT`.
    pub fn new(block: BlockId, count: u16) -> Option<Self> {
        if !block.is_carryable() || count == 0 || count > MAX_STACK_COUNT {
            return None;
        }
        Some(Self { block, count })
    }
}

/// Fixed-size legacy hotbar of optional block stacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hotbar {
    slots: [Option<ItemStack>; HOTBAR_SLOTS],
}

impl Hotbar {
    /// All slots empty.
    pub const fn empty() -> Self {
        Self {
            slots: [None; HOTBAR_SLOTS],
        }
    }

    /// Borrows a slot's stack, or `None` when the slot is empty or out of bounds.
    pub fn stack(&self, slot: usize) -> Option<ItemStack> {
        self.slots.get(slot).copied().flatten()
    }

    /// All slots, in display order.
    pub fn stacks(&self) -> &[Option<ItemStack>; HOTBAR_SLOTS] {
        &self.slots
    }

    /// Collects `amount` of `block` into the hotbar.
    ///
    /// Fills the first matching stack with room, then spills any remainder into
    /// later matching stacks and then empty slots. Counts never wrap: each
    /// slot is capped at `MAX_STACK_COUNT`. Returns the first slot written, or
    /// `None` when the block/amount is invalid or nothing can be accepted.
    pub fn collect(&mut self, block: BlockId, amount: u16) -> Option<usize> {
        if !block.is_carryable() || amount == 0 {
            return None;
        }

        let mut remaining = amount;
        let mut first_slot = None;

        // Top up existing matching stacks first.
        for slot in 0..HOTBAR_SLOTS {
            if remaining == 0 {
                break;
            }
            if let Some(stack) = self.slots[slot].as_mut() {
                if stack.block == block && stack.count < MAX_STACK_COUNT {
                    let room = MAX_STACK_COUNT - stack.count;
                    let added = remaining.min(room);
                    stack.count += added;
                    remaining -= added;
                    if first_slot.is_none() {
                        first_slot = Some(slot);
                    }
                }
            }
        }

        // Then spill the remainder into empty slots, one stack at a time.
        for slot in 0..HOTBAR_SLOTS {
            if remaining == 0 {
                break;
            }
            if self.slots[slot].is_none() {
                let added = remaining.min(MAX_STACK_COUNT);
                self.slots[slot] = ItemStack::new(block, added);
                remaining -= added;
                if first_slot.is_none() {
                    first_slot = Some(slot);
                }
            }
        }

        first_slot
    }

    /// Consumes one item from `slot`.
    ///
    /// Returns the consumed block id; clears the slot when the last item is
    /// taken. Out-of-bounds and empty slots return `None`.
    pub fn consume(&mut self, slot: usize) -> Option<BlockId> {
        let stack = self.slots.get_mut(slot)?.as_mut()?;
        let block = stack.block;
        if stack.count <= 1 {
            self.slots[slot] = None;
        } else {
            stack.count -= 1;
        }
        Some(block)
    }

    /// Rebuilds a hotbar from already-validated slots (persistence/tests).
    pub fn from_slots(slots: [Option<ItemStack>; HOTBAR_SLOTS]) -> Self {
        Self { slots }
    }
}

impl Default for Hotbar {
    fn default() -> Self {
        Self::empty()
    }
}

// ---------------------------------------------------------------------------
// Item identity and validated instances
// ---------------------------------------------------------------------------

/// Non-block materials produced by crafting or harvested from the world.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Material {
    Stick,
    Apple,
    Wheat,
    WheatSeeds,
    Bread,
    // Appended in this exact order: it is the append-only wire-tag order the
    // persistence layer depends on. Never reorder these variants.
    Leather,
    RawPorkchop,
    RawBeef,
    RottenFlesh,
    Bone,
    Arrow,
}

impl Material {
    pub const fn name(self) -> &'static str {
        match self {
            Material::Stick => "Stick",
            Material::Apple => "Apple",
            Material::Wheat => "Wheat",
            Material::WheatSeeds => "Wheat Seeds",
            Material::Bread => "Bread",
            Material::Leather => "Leather",
            Material::RawPorkchop => "Raw Porkchop",
            Material::RawBeef => "Raw Beef",
            Material::RottenFlesh => "Rotten Flesh",
            Material::Bone => "Bone",
            Material::Arrow => "Arrow",
        }
    }
}

/// Tool shape. Tools are always non-stackable and carry durability.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ToolKind {
    Pickaxe,
    Axe,
    Shovel,
    Hoe,
    // Appended after Hoe; these are the trades the melee/bow slices need.
    Sword,
    Bow,
}

impl ToolKind {
    pub const fn name(self) -> &'static str {
        match self {
            ToolKind::Pickaxe => "Pickaxe",
            ToolKind::Axe => "Axe",
            ToolKind::Shovel => "Shovel",
            ToolKind::Hoe => "Hoe",
            ToolKind::Sword => "Sword",
            ToolKind::Bow => "Bow",
        }
    }

    /// Tiers this kind is constructible in.
    ///
    /// The bow is deliberately wood-only (D133/MEM396): there is no stone or
    /// iron bow, so `(Bow, Stone)` and `(Bow, Iron)` must never become a valid
    /// [`Item`]. Swords exist in all three tiers.
    pub const fn allowed_tiers(self) -> &'static [ToolTier] {
        match self {
            ToolKind::Bow => &[ToolTier::Wood],
            ToolKind::Pickaxe
            | ToolKind::Axe
            | ToolKind::Shovel
            | ToolKind::Hoe
            | ToolKind::Sword => &[ToolTier::Wood, ToolTier::Stone, ToolTier::Iron],
        }
    }
}

/// Tool material tier, which fixes maximum durability.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ToolTier {
    Wood,
    Stone,
    Iron,
}

impl ToolTier {
    /// Maximum durability for the tier (Minecraft-equivalent values).
    pub const fn max_durability(self) -> u16 {
        match self {
            ToolTier::Wood => 59,
            ToolTier::Stone => 131,
            ToolTier::Iron => 250,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            ToolTier::Wood => "Wood",
            ToolTier::Stone => "Stone",
            ToolTier::Iron => "Iron",
        }
    }
}

/// Validated item identity.
///
/// Identity is distinct from an instance's remaining durability: two iron
/// pickaxes share this id but may have different [`Item::durability`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ItemId {
    Block(BlockId),
    Material(Material),
    Tool {
        kind: ToolKind,
        tier: ToolTier,
    },
    /// A registry-allocated pack generic item: a dense index into
    /// [`crate::content::ContentRegistry::items`].
    Item(u16),
    /// A registry-allocated pack tool: a dense index into
    /// [`crate::content::ContentRegistry::tools`].
    ModTool(u16),
}

impl ItemId {
    /// Maximum count this identity may occupy in one slot.
    pub const fn stack_limit(self) -> u16 {
        match self {
            ItemId::Block(_) => MAX_STACK_COUNT,
            ItemId::Material(_) => MATERIAL_STACK_LIMIT,
            ItemId::Tool { .. } => 1,
            ItemId::Item(_) => MAX_STACK_COUNT,
            ItemId::ModTool(_) => 1,
        }
    }

    pub const fn is_tool(self) -> bool {
        matches!(self, ItemId::Tool { .. } | ItemId::ModTool(_))
    }

    pub const fn tool(self) -> Option<(ToolKind, ToolTier)> {
        match self {
            ItemId::Tool { kind, tier } => Some((kind, tier)),
            _ => None,
        }
    }

    /// Human-readable label for HUD/log surfaces.
    ///
    /// The match is kind-first so only constructible identities are nameable:
    /// a bow has exactly one tier, and its name does not pretend otherwise.
    pub fn name(self) -> &'static str {
        match self {
            ItemId::Block(block) => block.name(),
            ItemId::Material(material) => material.name(),
            // A registry-allocated pack item has no static label; callers that
            // have the registry read the real display name from its `ItemDef`.
            ItemId::Item(_) => "Modded Item",
            // A pack tool index is not a `(kind, tier)` pair, so it has no
            // static kind-first label either; the registry names it.
            ItemId::ModTool(_) => "Modded Tool",
            ItemId::Tool { kind, tier } => match kind {
                ToolKind::Pickaxe => match tier {
                    ToolTier::Wood => "Wood Pickaxe",
                    ToolTier::Stone => "Stone Pickaxe",
                    ToolTier::Iron => "Iron Pickaxe",
                },
                ToolKind::Axe => match tier {
                    ToolTier::Wood => "Wood Axe",
                    ToolTier::Stone => "Stone Axe",
                    ToolTier::Iron => "Iron Axe",
                },
                ToolKind::Shovel => match tier {
                    ToolTier::Wood => "Wood Shovel",
                    ToolTier::Stone => "Stone Shovel",
                    ToolTier::Iron => "Iron Shovel",
                },
                ToolKind::Hoe => match tier {
                    ToolTier::Wood => "Wood Hoe",
                    ToolTier::Stone => "Stone Hoe",
                    ToolTier::Iron => "Iron Hoe",
                },
                ToolKind::Sword => match tier {
                    ToolTier::Wood => "Wood Sword",
                    ToolTier::Stone => "Stone Sword",
                    ToolTier::Iron => "Iron Sword",
                },
                ToolKind::Bow => "Wood Bow",
            },
        }
    }
}

/// Maps a worldgen loot identity onto the player item identity it yields.
///
/// Ores resolve to their block item; harvested plants resolve to the matching
/// material. This is the only bridge from [`LootItem`] into this module —
/// worldgen itself never imports the player layer.
impl From<LootItem> for ItemId {
    fn from(item: LootItem) -> Self {
        match item {
            LootItem::Apple => ItemId::Material(Material::Apple),
            LootItem::Bread => ItemId::Material(Material::Bread),
            LootItem::Wheat => ItemId::Material(Material::Wheat),
            LootItem::WheatSeeds => ItemId::Material(Material::WheatSeeds),
            LootItem::Stick => ItemId::Material(Material::Stick),
            LootItem::CoalOre => ItemId::Block(BlockId::CoalOre),
            LootItem::IronOre => ItemId::Block(BlockId::IronOre),
        }
    }
}

/// Maps a mob drop identity onto the player item identity it yields.
///
/// This is the only bridge from [`MobDropItem`] into this module — the world
/// layer's drop tables never import the player layer (D094). Bones and arrows
/// are materials; the rare bow resolves to the wood-tier tool identity, so
/// `Item::new` grants it full wood-bow durability (the bow is wood-only,
/// D133/MEM396).
///
/// The match is exhaustive by construction: a new [`MobDropItem`] variant is a
/// compile error here until it is given a real item identity.
impl From<MobDropItem> for ItemId {
    fn from(item: MobDropItem) -> Self {
        match item {
            MobDropItem::RawPorkchop => ItemId::Material(Material::RawPorkchop),
            MobDropItem::RawBeef => ItemId::Material(Material::RawBeef),
            MobDropItem::Leather => ItemId::Material(Material::Leather),
            MobDropItem::RottenFlesh => ItemId::Material(Material::RottenFlesh),
            MobDropItem::Bone => ItemId::Material(Material::Bone),
            MobDropItem::Arrow => ItemId::Material(Material::Arrow),
            MobDropItem::Bow => ItemId::Tool {
                kind: ToolKind::Bow,
                tier: ToolTier::Wood,
            },
        }
    }
}

/// Errors from item construction. Every variant is a rejected value, never a
/// partially built item.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ItemError {
    /// A count of zero is not an item.
    EmptyStack,
    /// Count exceeded the identity's stack limit.
    StackOverflow,
    /// Air, bedrock, and water cannot be carried.
    UncarryableBlock,
    /// Tools are non-stackable and must have count one.
    StackableTool,
    /// Durability was applied to, or requested from, a non-tool.
    NotATool,
    /// Tool durability must be `1..=tier.max_durability()`.
    InvalidDurability,
    /// The `(kind, tier)` pair is not a constructible tool identity, e.g. a
    /// stone or iron bow.
    TierNotAllowed,
    /// A registry identity was built without a registry. Pack items and pack
    /// tools must go through the `_with` constructors.
    RegistryRequired,
    /// A registry lookup failed: the index is not present in the registry.
    UnknownContent,
}

/// Shared tier gate for every tool constructor.
///
/// Both [`Item::new`] and [`Item::tool_with_durability`] route through this,
/// so an unconstructible pair such as `(Bow, Stone)` cannot be represented
/// even by a forged save record.
fn check_tool_tier(kind: ToolKind, tier: ToolTier) -> Result<(), ItemError> {
    if kind.allowed_tiers().contains(&tier) {
        Ok(())
    } else {
        Err(ItemError::TierNotAllowed)
    }
}

/// A validated item instance: identity, count, and (for tools) remaining
/// durability.
///
/// Fields are private so the only way to build one is through the checked
/// constructors below. Persistence decode must go through the same
/// constructors, which is what keeps forged bytes out of the domain.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Item {
    id: ItemId,
    count: u16,
    durability: Option<u16>,
    /// Maximum durability captured at construction. Built-in tiers derive it
    /// from `ToolTier`; pack tools store the authored `TierDef::durability`,
    /// so HUD and inventory labels stay registry-free.
    max_durability: Option<u16>,
}

impl Item {
    /// Builds a fully-repaired instance of `id` with `count` items.
    ///
    /// Blocks reject air/bedrock/water, materials enforce
    /// [`MATERIAL_STACK_LIMIT`], blocks enforce [`MAX_STACK_COUNT`], and tools
    /// must be count one (they start at full durability).
    pub fn new(id: ItemId, count: u16) -> Result<Self, ItemError> {
        match id {
            ItemId::Block(block) if !block.is_carryable() => Err(ItemError::UncarryableBlock),
            ItemId::Block(_) => Self::stackable(id, count, MAX_STACK_COUNT),
            ItemId::Item(_) => Self::stackable(id, count, MAX_STACK_COUNT),
            ItemId::Material(_) => Self::stackable(id, count, MATERIAL_STACK_LIMIT),
            ItemId::Tool { kind, tier } => {
                check_tool_tier(kind, tier)?;
                if count != 1 {
                    return Err(ItemError::StackableTool);
                }
                let max_durability = tier.max_durability();
                Ok(Self {
                    id,
                    count: 1,
                    durability: Some(max_durability),
                    max_durability: Some(max_durability),
                })
            }
            ItemId::ModTool(_) => Err(ItemError::RegistryRequired),
        }
    }

    /// Builds a fully-repaired instance of `id` against `registry`.
    ///
    /// Pack generic items use their authored stack limit and pack tools start
    /// at full authored durability. Built-in ids answer byte-identically to
    /// [`Item::new`] because the registry built-ins are derived from the same
    /// frozen tables.
    pub fn new_with(registry: &ContentRegistry, id: ItemId, count: u16) -> Result<Self, ItemError> {
        match id {
            ItemId::Block(block) => {
                if !block.is_carryable() {
                    return Err(ItemError::UncarryableBlock);
                }
                let limit = registry
                    .block(block)
                    .map_or(MAX_STACK_COUNT, |def| def.stack_limit)
                    .clamp(1, MAX_STACK_COUNT);
                Self::stackable(id, count, limit)
            }
            ItemId::Item(index) => {
                let def = registry.item(index).ok_or(ItemError::UnknownContent)?;
                let limit = def.stack_limit.clamp(1, MAX_STACK_COUNT);
                Self::stackable(id, count, limit)
            }
            ItemId::Material(_) => Self::stackable(id, count, MATERIAL_STACK_LIMIT),
            ItemId::Tool { kind, tier } => {
                check_tool_tier(kind, tier)?;
                if count != 1 {
                    return Err(ItemError::StackableTool);
                }
                let max_durability = tier.max_durability();
                Ok(Self {
                    id,
                    count: 1,
                    durability: Some(max_durability),
                    max_durability: Some(max_durability),
                })
            }
            ItemId::ModTool(index) => {
                let def = registry.tool(index).ok_or(ItemError::UnknownContent)?;
                let tier = registry.tier(def.tier).ok_or(ItemError::UnknownContent)?;
                if count != 1 {
                    return Err(ItemError::StackableTool);
                }
                let durability = tier.durability.max(1);
                Ok(Self {
                    id,
                    count: 1,
                    durability: Some(durability),
                    max_durability: Some(durability),
                })
            }
        }
    }

    fn stackable(id: ItemId, count: u16, limit: u16) -> Result<Self, ItemError> {
        if count == 0 {
            return Err(ItemError::EmptyStack);
        }
        if count > limit {
            return Err(ItemError::StackOverflow);
        }
        Ok(Self {
            id,
            count,
            durability: None,
            max_durability: None,
        })
    }

    /// Builds a tool with explicit remaining durability.
    ///
    /// This is the only entry point that accepts non-full durability and is
    /// intended for save decoding; it rejects zero and over-maximum values.
    pub fn tool_with_durability(
        kind: ToolKind,
        tier: ToolTier,
        durability: u16,
    ) -> Result<Self, ItemError> {
        check_tool_tier(kind, tier)?;
        if durability == 0 || durability > tier.max_durability() {
            return Err(ItemError::InvalidDurability);
        }
        Ok(Self {
            id: ItemId::Tool { kind, tier },
            count: 1,
            durability: Some(durability),
            max_durability: Some(tier.max_durability()),
        })
    }

    /// Builds a tool identity with explicit remaining durability against
    /// `registry`.
    ///
    /// Accepts a built-in `(kind, tier)` pair (same strict rule as
    /// [`Item::tool_with_durability`]) or a pack tool index, whose durability
    /// must be `1..=authored maximum`. Clamping is a persistence policy and is
    /// deliberately not performed here.
    pub fn tool_with_durability_with(
        registry: &ContentRegistry,
        id: ItemId,
        durability: u16,
    ) -> Result<Self, ItemError> {
        match id {
            ItemId::Tool { kind, tier } => Self::tool_with_durability(kind, tier, durability),
            ItemId::ModTool(index) => {
                let def = registry.tool(index).ok_or(ItemError::UnknownContent)?;
                let tier = registry.tier(def.tier).ok_or(ItemError::UnknownContent)?;
                let max_durability = tier.durability.max(1);
                if durability == 0 || durability > max_durability {
                    return Err(ItemError::InvalidDurability);
                }
                Ok(Self {
                    id,
                    count: 1,
                    durability: Some(durability),
                    max_durability: Some(max_durability),
                })
            }
            _ => Err(ItemError::NotATool),
        }
    }

    /// Converts a legacy block stack, reusing the same validation rules.
    pub fn from_block_stack(stack: ItemStack) -> Result<Self, ItemError> {
        Self::new(ItemId::Block(stack.block), stack.count)
    }

    pub const fn id(&self) -> ItemId {
        self.id
    }

    pub const fn count(&self) -> u16 {
        self.count
    }

    /// Remaining durability, or `None` for non-tools.
    pub const fn durability(&self) -> Option<u16> {
        self.durability
    }

    /// Maximum durability of this tool, or `None` for non-tools.
    pub fn max_durability(&self) -> Option<u16> {
        self.max_durability
    }

    pub const fn is_tool(&self) -> bool {
        self.id.is_tool()
    }

    /// Whether two instances of this identity may share a slot.
    pub fn is_stackable(&self) -> bool {
        self.id.stack_limit() > 1
    }

    /// Legacy block view, or `None` for non-block items.
    pub fn block_stack(&self) -> Option<ItemStack> {
        match self.id {
            ItemId::Block(block) => ItemStack::new(block, self.count),
            _ => None,
        }
    }

    /// Whether `other` can merge into this instance's stack.
    pub fn mergeable_with(&self, other: &Item) -> bool {
        self.id == other.id && self.is_stackable() && other.is_stackable()
    }

    /// Rebuilds this instance with a different count, revalidating the limit.
    ///
    /// Tools may only be rebuilt with count one; their durability is preserved.
    pub fn with_count(self, count: u16) -> Result<Self, ItemError> {
        if self.is_tool() {
            if count != 1 {
                return Err(ItemError::StackableTool);
            }
            return Ok(self);
        }
        Self::new(self.id, count)
    }

    /// Splits off half the stack, rounded up, for a "pick up half" gesture.
    ///
    /// Returns `(moved, remainder)`. A single item (including every tool)
    /// moves whole, and `remainder` is `None` when the source is emptied, so
    /// callers can never construct a zero-count item. Counts are conserved by
    /// construction: `moved.count + remainder.count == self.count`.
    pub fn split_half(self) -> (Item, Option<Item>) {
        if self.count <= 1 {
            return (self, None);
        }
        let moved = self.count / 2 + self.count % 2;
        let remainder = self.count - moved;
        (
            self.with_count(moved).expect("half stays within the limit"),
            Some(
                self.with_count(remainder)
                    .expect("remainder stays within the limit"),
            ),
        )
    }

    /// Splits off exactly one unit for a "place one" gesture.
    ///
    /// Returns `(moved, remainder)`. A single item or a non-stackable tool
    /// moves whole, and `remainder` is `None` when the source is emptied, so
    /// callers can never construct a zero-count item. Counts are conserved by
    /// construction: `moved.count + remainder.count == self.count`.
    pub fn split_one(self) -> (Item, Option<Item>) {
        if self.count <= 1 || !self.is_stackable() {
            return (self, None);
        }
        (
            self.with_count(1).expect("one stays within the limit"),
            Some(
                self.with_count(self.count - 1)
                    .expect("remainder stays within the limit"),
            ),
        )
    }

    /// Consumes one durability point.
    ///
    /// Returns `Ok(None)` when the tool broke (callers remove the slot) and
    /// `Err(ItemError::NotATool)` for non-tools, leaving the value untouched.
    pub fn use_tool_once(self) -> Result<Option<Item>, ItemError> {
        if !self.is_tool() {
            return Err(ItemError::NotATool);
        }
        let durability = self.durability.ok_or(ItemError::NotATool)?;
        if durability <= 1 {
            return Ok(None);
        }
        Ok(Some(Self {
            durability: Some(durability - 1),
            ..self
        }))
    }
}

// ---------------------------------------------------------------------------
// Inventory
// ---------------------------------------------------------------------------

/// Result of [`Inventory::collect_block`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CollectOutcome {
    /// First slot written (hotbar slots come first).
    pub first_slot: usize,
    /// Number of items actually accepted. May be less than requested when the
    /// inventory filled up; the caller keeps the remainder.
    pub accepted: u16,
}

/// Result of [`Inventory::collect_items`]: a best-effort batch with an
/// explicit accepted/lost report.
///
/// Unlike [`CollectOutcome`], this is not tied to a block identity and it
/// accounts for the whole batch, so callers can decide what to do with the
/// remainder instead of losing it silently.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CollectBatchOutcome {
    /// Total units moved into the inventory across every item in the batch.
    pub accepted: u32,
    /// Total units that did not fit and stayed with the caller.
    pub lost: u32,
}

impl CollectBatchOutcome {
    pub const fn new(accepted: u32, lost: u32) -> Self {
        Self { accepted, lost }
    }

    /// Whether any unit of the batch failed to fit.
    pub fn lost_any(&self) -> bool {
        self.lost > 0
    }
}

/// Errors from inventory mutation. Every error leaves the inventory exactly
/// as it was before the call.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InventoryError {
    OutOfBounds,
    EmptySlot,
    EmptyCursor,
    /// Cursor holds a different identity and cannot receive the slot stack.
    CursorOccupied,
    /// A "place one" right-click hit a slot holding a different identity.
    SlotOccupied,
    /// Cursor stack is already at its limit.
    CursorFull,
    /// Slot stack is already at its limit, so the cursor cannot merge into it.
    SlotFull,
    /// All 36 slots are full for this item.
    NoRoom,
    /// A durability operation targeted a non-tool.
    NotATool,
}

/// Result of wearing a tool.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ToolWear {
    /// Still usable with this remaining state (copied for convenience).
    Worn(Item),
    /// The tool reached zero durability and its slot was cleared.
    Broke,
}

/// Fixed 9 + 27 slot inventory with an optional cursor stack.
///
/// Slot index layout is flat and stable: `0..HOTBAR_SLOTS` are the hotbar,
/// `HOTBAR_SLOTS..INVENTORY_SLOTS` are main storage. Display/selection
/// semantics (the selected hotbar slot) stay with the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inventory {
    slots: [Option<Item>; INVENTORY_SLOTS],
    cursor: Option<Item>,
}

impl Inventory {
    /// All slots and the cursor empty.
    pub const fn empty() -> Self {
        Self {
            slots: [None; INVENTORY_SLOTS],
            cursor: None,
        }
    }

    /// Read-only view of all 36 slots in flat hotbar-then-main order.
    pub fn slots(&self) -> &[Option<Item>; INVENTORY_SLOTS] {
        &self.slots
    }

    /// Bounds-checked slot read.
    pub fn slot(&self, index: usize) -> Option<Item> {
        self.slots.get(index).copied().flatten()
    }

    /// Hotbar slot read (`slot` in `0..HOTBAR_SLOTS`).
    pub fn hotbar(&self, slot: usize) -> Option<Item> {
        if slot >= HOTBAR_SLOTS {
            return None;
        }
        self.slot(slot)
    }

    /// Main-storage slot read (`slot` in `0..MAIN_SLOTS`).
    pub fn main(&self, slot: usize) -> Option<Item> {
        if slot >= MAIN_SLOTS {
            return None;
        }
        self.slot(HOTBAR_SLOTS + slot)
    }

    pub fn cursor(&self) -> Option<Item> {
        self.cursor
    }

    /// Removes and returns the whole cursor stack.
    ///
    /// This is the conservation-safe primitive UI commands use for "hand to
    /// grid/cell": exactly the stack that was on the cursor comes back, and an
    /// empty cursor is an explicit [`InventoryError::EmptyCursor`] with no
    /// mutation. Callers must put the returned item somewhere or hand it back
    /// with [`Inventory::put_cursor`]; nothing here can duplicate or shrink it.
    pub fn take_cursor(&mut self) -> Result<Item, InventoryError> {
        self.cursor.take().ok_or(InventoryError::EmptyCursor)
    }

    /// Stores an already-validated item on an empty cursor, untouched.
    ///
    /// An occupied cursor is [`InventoryError::CursorOccupied`] with no
    /// mutation: this primitive never merges, splits, or swaps. Merging and
    /// swapping stay in [`Inventory::pick_up`] / [`Inventory::put_down`] /
    /// [`Inventory::swap_cursor`] so a UI click cannot silently change counts.
    pub fn put_cursor(&mut self, item: Item) -> Result<(), InventoryError> {
        if self.cursor.is_some() {
            return Err(InventoryError::CursorOccupied);
        }
        self.cursor = Some(item);
        Ok(())
    }

    /// Legacy read-only block view of a hotbar slot; `None` for empty,
    /// non-block, or out-of-range slots.
    pub fn hotbar_stack(&self, slot: usize) -> Option<ItemStack> {
        self.hotbar(slot).and_then(|item| item.block_stack())
    }

    /// Rebuilds from already-validated parts (persistence decode/tests).
    ///
    /// Items can only exist through [`Item`]'s checked constructors, so this
    /// cannot introduce forged values.
    pub fn from_parts(slots: [Option<Item>; INVENTORY_SLOTS], cursor: Option<Item>) -> Self {
        Self { slots, cursor }
    }

    /// Lossless migration from a legacy v1-v3 hotbar; main slots and cursor
    /// start empty. Legacy hotbar entries were already validated.
    pub fn from_legacy_hotbar(hotbar: &Hotbar) -> Self {
        let mut inventory = Self::empty();
        for (slot, stack) in hotbar.stacks().iter().enumerate() {
            if let Some(stack) = stack {
                if let Ok(item) = Item::from_block_stack(*stack) {
                    inventory.slots[slot] = Some(item);
                }
            }
        }
        inventory
    }

    /// Legacy hotbar view, or `None` when the hotbar holds a non-block item
    /// that the legacy representation cannot express.
    pub fn to_legacy_hotbar(&self) -> Option<Hotbar> {
        let mut slots = [None; HOTBAR_SLOTS];
        for (slot, item) in slots.iter_mut().zip(self.slots.iter()) {
            if let Some(item) = item {
                *slot = Some(item.block_stack()?);
            }
        }
        Some(Hotbar::from_slots(slots))
    }

    /// Whether `item` fits entirely right now (merging into compatible
    /// stacks first, then empty slots).
    pub fn can_insert(&self, item: &Item) -> bool {
        let limit = u32::from(item.id.stack_limit());
        let mut room: u32 = 0;
        for existing in self.slots.iter().flatten() {
            if existing.mergeable_with(item) {
                room += limit - u32::from(existing.count);
            }
        }
        let empty = self.slots.iter().filter(|slot| slot.is_none()).count() as u32;
        room += empty * limit;
        room >= u32::from(item.count)
    }

    /// Inserts `item` completely or not at all.
    ///
    /// All-or-nothing keeps world pickup from silently dropping items: when
    /// there is no room the caller still owns the whole stack.
    pub fn insert(&mut self, item: Item) -> Result<(), InventoryError> {
        if !self.can_insert(&item) {
            return Err(InventoryError::NoRoom);
        }

        let limit = item.id.stack_limit();
        let mut remaining = item.count;

        if item.is_stackable() {
            for existing in self.slots.iter_mut().flatten() {
                if remaining == 0 {
                    break;
                }
                if existing.mergeable_with(&item) && existing.count < limit {
                    let moved = remaining.min(limit - existing.count);
                    existing.count += moved;
                    remaining -= moved;
                }
            }
            for slot in self.slots.iter_mut() {
                if remaining == 0 {
                    break;
                }
                if slot.is_none() {
                    let moved = remaining.min(limit);
                    *slot = Some(item.with_count(moved).expect("partial count fits"));
                    remaining -= moved;
                }
            }
        } else {
            let slot = self
                .slots
                .iter_mut()
                .find(|slot| slot.is_none())
                .expect("can_insert guaranteed an empty slot");
            *slot = Some(item);
            remaining = 0;
        }

        debug_assert_eq!(remaining, 0, "insert must consume the whole stack");
        Ok(())
    }

    /// Grants one item into exactly one hotbar slot for Creative palette use.
    ///
    /// This is deliberately **not** a conservation path: the item is placed
    /// wholesale, never merged, and its count is never changed. The write
    /// priority is the first empty hotbar slot, then an existing hotbar stack
    /// of the same identity (replaced wholesale), then `preferred` clamped
    /// into the hotbar when all nine slots are occupied. Main storage
    /// (`HOTBAR_SLOTS..INVENTORY_SLOTS`), the cursor, and both crafting grids
    /// are never touched. Returns the slot that now holds `item`.
    pub fn grant_hotbar(&mut self, item: Item, preferred: usize) -> usize {
        let hotbar = &mut self.slots[..HOTBAR_SLOTS];
        let slot = hotbar
            .iter()
            .position(Option::is_none)
            .or_else(|| {
                hotbar
                    .iter()
                    .position(|existing| matches!(existing, Some(stack) if stack.id() == item.id()))
            })
            .unwrap_or_else(|| preferred.min(HOTBAR_SLOTS - 1));
        hotbar[slot] = Some(item);
        slot
    }

    /// Whether every item in `items` fits, in order, without touching the
    /// cursor or either crafting grid.
    ///
    /// This is a trial run of [`Inventory::insert_all`]: the items compete for
    /// the same mergeable stacks and empty slots, so per-item `can_insert`
    /// checks are not equivalent when the list contains several stacks of the
    /// same identity.
    pub fn can_insert_all(&self, items: &[Item]) -> bool {
        let mut trial = self.clone();
        items.iter().all(|item| trial.insert(*item).is_ok())
    }

    /// Inserts every item completely or none at all.
    ///
    /// Works on a clone and commits the whole slot array only after the last
    /// item inserts, which is what makes the chest-break preflight atomic for
    /// heterogeneous contents plus the chest's own drop item. The cursor and
    /// both crafting grids are never part of the trial or the commit.
    pub fn insert_all(&mut self, items: &[Item]) -> Result<(), InventoryError> {
        let mut trial = self.clone();
        for item in items {
            trial.insert(*item)?;
        }
        // A full inventory clone is cheap (36 copyable slots) and committing
        // the slots keeps the cursor exactly as the caller left it.
        self.slots = trial.slots;
        Ok(())
    }

    /// Best-effort block collection in hotbar-then-main order.
    ///
    /// Unlike [`Inventory::insert`], this matches world pickup semantics: it
    /// accepts what fits and reports how much it took so the caller can keep
    /// the remainder. Returns `None` (no mutation) when nothing was accepted
    /// or the block cannot be carried.
    pub fn collect_block(&mut self, block: BlockId, amount: u16) -> Option<CollectOutcome> {
        if !block.is_carryable() {
            return None;
        }
        self.collect_limited(ItemId::Block(block), amount, MAX_STACK_COUNT)
    }

    /// Best-effort collection of any [`ItemId`] under an authored stack limit.
    ///
    /// This is [`Inventory::collect_block`] with the identity and per-slot cap
    /// supplied by the caller (the session registry's [`crate::content::drop_entry`]
    /// for pack-authored drops). Compatible existing stacks merge first, then
    /// empty slots fill in flat hotbar-then-main order. `limit` is clamped to
    /// `1..=MAX_STACK_COUNT` and then to the identity's own cap so a slot can
    /// never hold an invalid stack. Returns `None` with no mutation when the
    /// amount is zero or nothing fits.
    pub fn collect_limited(
        &mut self,
        id: ItemId,
        amount: u16,
        limit: u16,
    ) -> Option<CollectOutcome> {
        self.collect_limited_impl(None, id, amount, limit)
    }

    /// Registry-aware [`Inventory::collect_limited`].
    ///
    /// Pack identities are constructed through [`Item::new_with`], so an
    /// authored drop can be a generic item or a non-stackable pack tool even
    /// though [`Item::new`] deliberately refuses registry identities. The
    /// merge/fill order, clamping and no-room contract are identical to the
    /// registry-free path, and built-in ids answer byte-identically.
    pub fn collect_limited_with(
        &mut self,
        registry: &ContentRegistry,
        id: ItemId,
        amount: u16,
        limit: u16,
    ) -> Option<CollectOutcome> {
        self.collect_limited_impl(Some(registry), id, amount, limit)
    }

    fn collect_limited_impl(
        &mut self,
        registry: Option<&ContentRegistry>,
        id: ItemId,
        amount: u16,
        limit: u16,
    ) -> Option<CollectOutcome> {
        if amount == 0 {
            return None;
        }

        let limit = limit.clamp(1, MAX_STACK_COUNT).min(id.stack_limit()).max(1);
        let mut remaining = amount;
        let mut first_slot = None;

        for (index, slot) in self.slots.iter_mut().enumerate() {
            if remaining == 0 {
                break;
            }
            if let Some(existing) = slot {
                if existing.id() == id && existing.count < limit {
                    let moved = remaining.min(limit - existing.count);
                    existing.count += moved;
                    remaining -= moved;
                    if first_slot.is_none() {
                        first_slot = Some(index);
                    }
                }
            }
        }

        for (index, slot) in self.slots.iter_mut().enumerate() {
            if remaining == 0 {
                break;
            }
            if slot.is_none() {
                let moved = remaining.min(limit);
                let item = match registry {
                    Some(registry) => Item::new_with(registry, id, moved),
                    None => Item::new(id, moved),
                };
                if let Ok(item) = item {
                    *slot = Some(item);
                    remaining -= moved;
                    if first_slot.is_none() {
                        first_slot = Some(index);
                    }
                }
            }
        }

        first_slot.map(|first_slot| CollectOutcome {
            first_slot,
            accepted: amount - remaining,
        })
    }

    /// Best-effort batch collection in flat hotbar-then-main order.
    ///
    /// This is the best-effort sibling of [`Inventory::insert`] and
    /// [`Inventory::insert_all`], whose all-or-nothing contracts the chest
    /// break and wheat-harvest preflights depend on. Every stackable item
    /// merges into compatible existing stacks first (never past
    /// [`ItemId::stack_limit`]) and then fills empty slots, splitting with
    /// [`Item::with_count`]; a non-stackable tool takes at most one empty slot
    /// as a whole. Whatever still does not fit is counted in
    /// [`CollectBatchOutcome::lost`] and left for the caller, so a full
    /// inventory loses only the overflow.
    ///
    /// This never touches [`Inventory::cursor`] and the crafting grids live
    /// outside the aggregate, so neither can be a batch destination. Units are
    /// conserved per batch: `accepted + lost == sum(items.count)`.
    pub fn collect_items(&mut self, items: &[Item]) -> CollectBatchOutcome {
        let mut accepted: u32 = 0;
        let mut lost: u32 = 0;

        for item in items {
            if item.is_stackable() {
                let limit = item.id.stack_limit();
                let mut remaining = item.count;

                for existing in self.slots.iter_mut().flatten() {
                    if remaining == 0 {
                        break;
                    }
                    if existing.mergeable_with(item) && existing.count < limit {
                        let moved = remaining.min(limit - existing.count);
                        existing.count += moved;
                        remaining -= moved;
                    }
                }

                for slot in self.slots.iter_mut() {
                    if remaining == 0 {
                        break;
                    }
                    if slot.is_none() {
                        let moved = remaining.min(limit);
                        *slot = Some(
                            item.with_count(moved)
                                .expect("partial count stays within the limit"),
                        );
                        remaining -= moved;
                    }
                }

                accepted += u32::from(item.count - remaining);
                lost += u32::from(remaining);
            } else if let Some(slot) = self.slots.iter_mut().find(|slot| slot.is_none()) {
                *slot = Some(*item);
                accepted += u32::from(item.count);
            } else {
                lost += u32::from(item.count);
            }
        }

        CollectBatchOutcome::new(accepted, lost)
    }

    /// Consumes one unit from `index`, returning it. Clears the slot at zero.
    ///
    /// Out-of-bounds and empty slots return `None` without mutation.
    pub fn consume_one(&mut self, index: usize) -> Option<Item> {
        let slot = self.slots.get_mut(index)?;
        let item = slot.as_mut()?;
        let consumed = item.with_count(1).ok()?;
        if item.count == 1 {
            *slot = None;
        } else {
            item.count -= 1;
        }
        Some(consumed)
    }

    /// Consumes one unit of `id` from the first matching slot, returning the
    /// consumed unit and its slot index.
    ///
    /// Slots are scanned in their existing flat hotbar-then-main order, so a
    /// stack sitting in the hotbar is always spent before an identical stack
    /// in main storage. Exactly one unit is removed: a multi-unit stack shrinks
    /// by one, and a single-unit slot is cleared. Neither
    /// [`Inventory::cursor`] nor either crafting grid (which live outside this
    /// aggregate) is ever a source or a destination.
    ///
    /// An identity held nowhere in the 36 slots returns `None` with every
    /// slot unchanged. This is the by-identity sibling of
    /// [`Inventory::consume_one`], which requires an already-known index: the
    /// bow occupies the selected hotbar slot, so spending an arrow must not
    /// disturb the selection.
    pub fn consume_item(&mut self, id: ItemId) -> Option<(usize, Item)> {
        let index = self
            .slots
            .iter()
            .position(|slot| slot.is_some_and(|item| item.id() == id))?;
        let consumed = self.consume_one(index)?;
        Some((index, consumed))
    }

    /// Picks the whole slot stack up onto the cursor.
    ///
    /// - Empty cursor: the stack moves to the cursor.
    /// - Matching stackable item: merges as much as the cursor limit allows
    ///   (partial merges leave the remainder in the slot).
    /// - Different identity or a tool: [`InventoryError::CursorOccupied`].
    /// - Cursor already full: [`InventoryError::CursorFull`].
    pub fn pick_up(&mut self, index: usize) -> Result<(), InventoryError> {
        let slot_item = self
            .slots
            .get(index)
            .copied()
            .ok_or(InventoryError::OutOfBounds)?
            .ok_or(InventoryError::EmptySlot)?;

        match self.cursor {
            None => {
                self.slots[index] = None;
                self.cursor = Some(slot_item);
                Ok(())
            }
            Some(cursor) => {
                if !cursor.mergeable_with(&slot_item) {
                    return Err(InventoryError::CursorOccupied);
                }
                let limit = cursor.id.stack_limit();
                if cursor.count >= limit {
                    return Err(InventoryError::CursorFull);
                }
                let moved = slot_item.count.min(limit - cursor.count);
                self.cursor = Some(
                    cursor
                        .with_count(cursor.count + moved)
                        .expect("merge stays within the stack limit"),
                );
                if moved == slot_item.count {
                    self.slots[index] = None;
                } else {
                    self.slots[index] = Some(
                        slot_item
                            .with_count(slot_item.count - moved)
                            .expect("merge remainder stays valid"),
                    );
                }
                Ok(())
            }
        }
    }

    /// Picks up half the slot stack (rounded up) onto an empty cursor.
    ///
    /// A single item or tool moves whole. An occupied cursor is
    /// [`InventoryError::CursorOccupied`] with no mutation, so a right-click
    /// can never merge into or replace an existing hand stack.
    pub fn pick_up_half(&mut self, index: usize) -> Result<(), InventoryError> {
        let slot_item = self
            .slots
            .get(index)
            .copied()
            .ok_or(InventoryError::OutOfBounds)?
            .ok_or(InventoryError::EmptySlot)?;
        if self.cursor.is_some() {
            return Err(InventoryError::CursorOccupied);
        }

        let (moved, remainder) = slot_item.split_half();
        self.slots[index] = remainder;
        self.cursor = Some(moved);
        Ok(())
    }

    /// Puts exactly one unit from the cursor into a slot.
    ///
    /// An empty slot accepts one item (a whole tool moves in), and a matching
    /// stackable slot receives one item up to its limit. A different identity
    /// is [`InventoryError::SlotOccupied`], a full matching stack is
    /// [`InventoryError::SlotFull`], and both leave the inventory unchanged.
    /// Every success conserves the combined item count.
    pub fn place_one(&mut self, index: usize) -> Result<(), InventoryError> {
        let cursor = self.cursor.ok_or(InventoryError::EmptyCursor)?;
        let slot_item = self
            .slots
            .get(index)
            .copied()
            .ok_or(InventoryError::OutOfBounds)?;

        match slot_item {
            None => {
                let (moved, remainder) = cursor.split_one();
                self.slots[index] = Some(moved);
                self.cursor = remainder;
                Ok(())
            }
            Some(slot_item) => {
                if !cursor.mergeable_with(&slot_item) {
                    return Err(InventoryError::SlotOccupied);
                }
                let limit = slot_item.id.stack_limit();
                if slot_item.count >= limit {
                    return Err(InventoryError::SlotFull);
                }
                let (moved, remainder) = cursor.split_one();
                self.slots[index] = Some(
                    slot_item
                        .with_count(slot_item.count + moved.count)
                        .expect("merge stays within the stack limit"),
                );
                self.cursor = remainder;
                Ok(())
            }
        }
    }

    /// Puts the cursor stack down into a slot.
    ///
    /// - Empty slot: the cursor moves in.
    /// - Matching stackable item: merges into the slot up to its limit; the
    ///   remainder stays on the cursor.
    /// - Different identity or a tool: swaps cursor and slot.
    /// - Full matching slot: [`InventoryError::SlotFull`].
    pub fn put_down(&mut self, index: usize) -> Result<(), InventoryError> {
        let cursor = self.cursor.ok_or(InventoryError::EmptyCursor)?;
        let slot_item = self
            .slots
            .get(index)
            .copied()
            .ok_or(InventoryError::OutOfBounds)?;

        match slot_item {
            None => {
                self.slots[index] = Some(cursor);
                self.cursor = None;
                Ok(())
            }
            Some(slot_item) => {
                if cursor.mergeable_with(&slot_item) {
                    let limit = cursor.id.stack_limit();
                    if slot_item.count >= limit {
                        return Err(InventoryError::SlotFull);
                    }
                    let moved = cursor.count.min(limit - slot_item.count);
                    self.slots[index] = Some(
                        slot_item
                            .with_count(slot_item.count + moved)
                            .expect("merge stays within the stack limit"),
                    );
                    if moved == cursor.count {
                        self.cursor = None;
                    } else {
                        self.cursor = Some(
                            cursor
                                .with_count(cursor.count - moved)
                                .expect("merge remainder stays valid"),
                        );
                    }
                    Ok(())
                } else {
                    self.slots[index] = Some(cursor);
                    self.cursor = Some(slot_item);
                    Ok(())
                }
            }
        }
    }

    /// Explicitly swaps the cursor with a slot's contents.
    ///
    /// Requires both sides occupied; use [`Inventory::put_down`] for merges
    /// and empty-slot placement. Swapping two instances of the same identity
    /// (e.g. tools with different durability) is allowed.
    pub fn swap_cursor(&mut self, index: usize) -> Result<(), InventoryError> {
        let cursor = self.cursor.ok_or(InventoryError::EmptyCursor)?;
        let slot_item = self
            .slots
            .get(index)
            .copied()
            .ok_or(InventoryError::OutOfBounds)?
            .ok_or(InventoryError::EmptySlot)?;
        self.slots[index] = Some(cursor);
        self.cursor = Some(slot_item);
        Ok(())
    }

    /// Consumes one durability point from the tool at `index`.
    ///
    /// A tool that reaches zero is removed from the slot and reported as
    /// [`ToolWear::Broke`]. Non-tools and empty/bounds-missed slots are
    /// explicit errors with no mutation.
    pub fn use_tool(&mut self, index: usize) -> Result<ToolWear, InventoryError> {
        let item = self
            .slots
            .get(index)
            .copied()
            .ok_or(InventoryError::OutOfBounds)?
            .ok_or(InventoryError::EmptySlot)?;
        let worn = item.use_tool_once().map_err(|_| InventoryError::NotATool)?;
        match worn {
            Some(next) => {
                self.slots[index] = Some(next);
                Ok(ToolWear::Worn(next))
            }
            None => {
                self.slots[index] = None;
                Ok(ToolWear::Broke)
            }
        }
    }
}

impl Default for Inventory {
    fn default() -> Self {
        Self::empty()
    }
}

// ---------------------------------------------------------------------------
// Crafting
// ---------------------------------------------------------------------------

/// Which crafting surface (and therefore grid size) is in use.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GridKind {
    /// 2x2 grid available from the player inventory.
    Player,
    /// 4x4 grid available from a placed crafting table.
    Table,
}

impl GridKind {
    pub const fn dim(self) -> usize {
        match self {
            GridKind::Player => PLAYER_GRID_DIM,
            GridKind::Table => TABLE_GRID_DIM,
        }
    }

    /// Number of active cells in this grid.
    pub const fn slot_count(self) -> usize {
        self.dim() * self.dim()
    }
}

/// Stable recipe identifiers for tests, persistence-adjacent UI, and logs.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RecipeId {
    Planks,
    Sticks,
    CraftingTable,
    Chest,
    WoodPickaxe,
    WoodAxe,
    WoodShovel,
    StonePickaxe,
    StoneAxe,
    StoneShovel,
    IronPickaxe,
    IronAxe,
    IronShovel,
    Bread,
    WoodHoe,
    StoneHoe,
    IronHoe,
    WoodSword,
    StoneSword,
    IronSword,
    Shortbow,
    Arrows,
    /// A registry-allocated pack recipe: a dense index into
    /// [`crate::content::ContentRegistry::recipes`] (`>= 22`).
    Modded(u16),
}

/// Read-only recipe evaluation result.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RecipePreview {
    pub recipe: RecipeId,
    pub output: ItemId,
    pub output_count: u16,
}

/// Errors from grid mutation and crafting. Every error leaves the grid and the
/// destination inventory unchanged (crafting validates before consuming).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CraftError {
    /// Cell index is outside the active grid.
    OutOfBounds,
    /// A construction attempt supplied a value in the inactive storage region.
    InactiveCell,
    /// The current grid contents match no recipe.
    NoRecipe,
    /// A "place one" right-click hit an occupied cell holding a different
    /// identity.
    CellOccupied,
    /// A "place one" right-click hit a cell already at its stack limit.
    CellFull,
    /// The destination inventory cannot hold the recipe output right now.
    DestinationFull,
}

/// Shaped crafting recipe. Each non-space pattern character is one item
/// identity and one consumed grid cell; the pattern may sit anywhere in the
/// grid as long as its trimmed bounding box matches.
#[derive(Copy, Clone)]
pub(crate) struct Recipe {
    pub(crate) id: RecipeId,
    pub(crate) pattern: &'static [&'static str],
    pub(crate) output: ItemId,
    pub(crate) output_count: u16,
}

/// Shaped recipe table.
///
/// The material chain is grounded in what the world actually yields today:
/// oak logs become planks, planks become sticks and the crafting table, and
/// the three tool tiers use planks, cobblestone, and mined iron ore as their
/// head material. Adding smelting later can swap the iron head identity
/// without changing the shaped-recipe machinery.
pub(crate) const RECIPES: &[Recipe] = &[
    Recipe {
        id: RecipeId::Planks,
        pattern: &["L"],
        output: ItemId::Block(BlockId::OakPlanks),
        output_count: 4,
    },
    Recipe {
        id: RecipeId::Sticks,
        pattern: &["P", "P"],
        output: ItemId::Material(Material::Stick),
        output_count: 4,
    },
    Recipe {
        id: RecipeId::CraftingTable,
        pattern: &["PP", "PP"],
        output: ItemId::Block(BlockId::CraftingTable),
        output_count: 1,
    },
    Recipe {
        id: RecipeId::Chest,
        pattern: &["PPP", "P P", "PPP"],
        output: ItemId::Block(BlockId::Chest),
        output_count: 1,
    },
    Recipe {
        id: RecipeId::WoodPickaxe,
        pattern: &["PPP", " S ", " S "],
        output: ItemId::Tool {
            kind: ToolKind::Pickaxe,
            tier: ToolTier::Wood,
        },
        output_count: 1,
    },
    Recipe {
        id: RecipeId::WoodAxe,
        pattern: &["PP ", "PS ", " S "],
        output: ItemId::Tool {
            kind: ToolKind::Axe,
            tier: ToolTier::Wood,
        },
        output_count: 1,
    },
    Recipe {
        id: RecipeId::WoodShovel,
        pattern: &["P", "S", "S"],
        output: ItemId::Tool {
            kind: ToolKind::Shovel,
            tier: ToolTier::Wood,
        },
        output_count: 1,
    },
    Recipe {
        id: RecipeId::StonePickaxe,
        pattern: &["CCC", " S ", " S "],
        output: ItemId::Tool {
            kind: ToolKind::Pickaxe,
            tier: ToolTier::Stone,
        },
        output_count: 1,
    },
    Recipe {
        id: RecipeId::StoneAxe,
        pattern: &["CC ", "CS ", " S "],
        output: ItemId::Tool {
            kind: ToolKind::Axe,
            tier: ToolTier::Stone,
        },
        output_count: 1,
    },
    Recipe {
        id: RecipeId::StoneShovel,
        pattern: &["C", "S", "S"],
        output: ItemId::Tool {
            kind: ToolKind::Shovel,
            tier: ToolTier::Stone,
        },
        output_count: 1,
    },
    Recipe {
        id: RecipeId::IronPickaxe,
        pattern: &["III", " S ", " S "],
        output: ItemId::Tool {
            kind: ToolKind::Pickaxe,
            tier: ToolTier::Iron,
        },
        output_count: 1,
    },
    Recipe {
        id: RecipeId::IronAxe,
        pattern: &["II ", "IS ", " S "],
        output: ItemId::Tool {
            kind: ToolKind::Axe,
            tier: ToolTier::Iron,
        },
        output_count: 1,
    },
    Recipe {
        id: RecipeId::IronShovel,
        pattern: &["I", "S", "S"],
        output: ItemId::Tool {
            kind: ToolKind::Shovel,
            tier: ToolTier::Iron,
        },
        output_count: 1,
    },
    Recipe {
        id: RecipeId::Bread,
        pattern: &["WWW"],
        output: ItemId::Material(Material::Bread),
        output_count: 1,
    },
    // Hoes are 2 wide and 3 tall, so they only fit the 4x4 table and stay
    // visually distinct from the 3-wide pickaxe/axe heads and the 1-wide
    // shovel column.
    Recipe {
        id: RecipeId::WoodHoe,
        pattern: &["PP", " S", " S"],
        output: ItemId::Tool {
            kind: ToolKind::Hoe,
            tier: ToolTier::Wood,
        },
        output_count: 1,
    },
    Recipe {
        id: RecipeId::StoneHoe,
        pattern: &["CC", " S", " S"],
        output: ItemId::Tool {
            kind: ToolKind::Hoe,
            tier: ToolTier::Stone,
        },
        output_count: 1,
    },
    Recipe {
        id: RecipeId::IronHoe,
        pattern: &["II", " S", " S"],
        output: ItemId::Tool {
            kind: ToolKind::Hoe,
            tier: ToolTier::Iron,
        },
        output_count: 1,
    },
    // Swords follow the vanilla 1-wide, 3-tall head-over-handle column. Like
    // the hoes they structurally cannot fit the 2x2 player grid, so they are
    // table-only by construction rather than by an explicit grid check.
    Recipe {
        id: RecipeId::WoodSword,
        pattern: &["P", "P", "S"],
        output: ItemId::Tool {
            kind: ToolKind::Sword,
            tier: ToolTier::Wood,
        },
        output_count: 1,
    },
    Recipe {
        id: RecipeId::StoneSword,
        pattern: &["C", "C", "S"],
        output: ItemId::Tool {
            kind: ToolKind::Sword,
            tier: ToolTier::Stone,
        },
        output_count: 1,
    },
    Recipe {
        id: RecipeId::IronSword,
        pattern: &["I", "I", "S"],
        output: ItemId::Tool {
            kind: ToolKind::Sword,
            tier: ToolTier::Iron,
        },
        output_count: 1,
    },
    // Ranged entry points. The bow is 3 wide and 3 tall (2 leather, 3 sticks)
    // so it is table-only by shape, exactly like the hoes and swords. Arrows
    // are a 1x2 column so the same recipe also fits the 2x2 player grid: a
    // player who has never placed a table can still make ammo.
    Recipe {
        id: RecipeId::Shortbow,
        pattern: &[" SE", "S E", " S "],
        output: ItemId::Tool {
            kind: ToolKind::Bow,
            tier: ToolTier::Wood,
        },
        output_count: 1,
    },
    Recipe {
        id: RecipeId::Arrows,
        pattern: &["S", "K"],
        output: ItemId::Material(Material::Arrow),
        output_count: 4,
    },
];

/// Pattern character to item identity. Unknown characters never match.
///
/// Known characters: `L` oak log, `P` oak planks, `C` cobblestone,
/// `I` iron ore, `S` stick, `W` wheat, `E` leather, `K` coal ore.
///
/// There is no `Material::Coal` in this codebase, so the coal ore block is the
/// only coal identity available — the same convention `I` already uses for
/// iron ore. Smelting can later swap both head materials without touching the
/// shaped-recipe machinery.
pub(crate) fn pattern_item(byte: u8) -> Option<ItemId> {
    match byte {
        b'L' => Some(ItemId::Block(BlockId::OakLog)),
        b'P' => Some(ItemId::Block(BlockId::OakPlanks)),
        b'C' => Some(ItemId::Block(BlockId::Cobblestone)),
        b'I' => Some(ItemId::Block(BlockId::IronOre)),
        b'S' => Some(ItemId::Material(Material::Stick)),
        b'W' => Some(ItemId::Material(Material::Wheat)),
        b'E' => Some(ItemId::Material(Material::Leather)),
        b'K' => Some(ItemId::Block(BlockId::CoalOre)),
        _ => None,
    }
}

/// Bounding box of non-empty cells in a grid or pattern.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Bounds {
    top: usize,
    left: usize,
    height: usize,
    width: usize,
}

fn grid_bounds(grid: &CraftingGrid) -> Option<Bounds> {
    let dim = grid.kind.dim();
    let mut top = usize::MAX;
    let mut left = usize::MAX;
    let mut bottom = 0;
    let mut right = 0;
    for (index, cell) in grid.slots.iter().take(dim * dim).enumerate() {
        if cell.is_some() {
            let row = index / dim;
            let col = index % dim;
            top = top.min(row);
            left = left.min(col);
            bottom = bottom.max(row);
            right = right.max(col);
        }
    }
    if top == usize::MAX {
        return None;
    }
    Some(Bounds {
        top,
        left,
        height: bottom - top + 1,
        width: right - left + 1,
    })
}

fn pattern_bounds(pattern: &[String]) -> Option<Bounds> {
    let mut top = usize::MAX;
    let mut left = usize::MAX;
    let mut bottom = 0;
    let mut right = 0;
    for (row, line) in pattern.iter().enumerate() {
        for (col, byte) in line.bytes().enumerate() {
            if byte != b' ' {
                top = top.min(row);
                left = left.min(col);
                bottom = bottom.max(row);
                right = right.max(col);
            }
        }
    }
    if top == usize::MAX {
        return None;
    }
    Some(Bounds {
        top,
        left,
        height: bottom - top + 1,
        width: right - left + 1,
    })
}

/// Registry-aware shaped match.
///
/// Compares the trimmed grid bounds against the trimmed pattern bounds, then
/// every pattern cell: spaces must be empty, and every key character must
/// resolve through the registry to the item identity currently in the cell.
/// An absent character, an unknown index, or placeholder content fails closed.
fn matches_pattern(
    grid: &CraftingGrid,
    grid_bounds: Bounds,
    registry: &ContentRegistry,
    recipe: &RecipeDef,
) -> bool {
    let Some(pattern_bounds) = pattern_bounds(&recipe.pattern) else {
        return false;
    };
    if pattern_bounds.width != grid_bounds.width || pattern_bounds.height != grid_bounds.height {
        return false;
    }

    let dim = grid.kind.dim();
    for row in 0..pattern_bounds.height {
        let line = recipe.pattern[pattern_bounds.top + row].as_bytes();
        for col in 0..pattern_bounds.width {
            let Some(expected) = line.get(pattern_bounds.left + col).copied() else {
                return false;
            };
            let actual = grid.slot((grid_bounds.top + row) * dim + grid_bounds.left + col);
            if expected == b' ' {
                if actual.is_some() {
                    return false;
                }
            } else {
                let Some(reference) = recipe.key.get(&char::from(expected)) else {
                    return false;
                };
                let Some(expected_id) = registry.resolve_item_ref(*reference) else {
                    return false;
                };
                if actual.map(|item| item.id()) != Some(expected_id) {
                    return false;
                }
            }
        }
    }
    true
}

fn recipe_for<'a>(grid: &CraftingGrid, registry: &'a ContentRegistry) -> Option<&'a RecipeDef> {
    let grid_bounds = grid_bounds(grid)?;
    registry
        .enabled_recipes()
        .map(|(_, recipe)| recipe)
        .find(|recipe| {
            registry.resolve_item_ref(recipe.output).is_some()
                && matches_pattern(grid, grid_bounds, registry, recipe)
        })
}

/// Map a dense registry recipe index back to the frozen public id: the 22
/// built-ins keep their names, every pack recipe is [`RecipeId::Modded`].
fn recipe_id_for_index(index: u16) -> RecipeId {
    names::builtin_recipe_by_index(index).unwrap_or(RecipeId::Modded(index))
}

/// A bounded crafting grid. Only the active `dim * dim` region is addressable;
/// the fixed storage is sized for the largest supported grid (4x4), so a 2x2
/// player grid leaves its tail inactive. Inactive cells are validated to stay
/// empty when decoding persisted state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CraftingGrid {
    kind: GridKind,
    slots: [Option<Item>; MAX_GRID_SLOTS],
}

impl CraftingGrid {
    /// Empty grid of the given kind.
    pub const fn new(kind: GridKind) -> Self {
        Self {
            kind,
            slots: [None; MAX_GRID_SLOTS],
        }
    }

    pub const fn kind(&self) -> GridKind {
        self.kind
    }

    pub const fn dim(&self) -> usize {
        self.kind.dim()
    }

    /// Bounds-checked read within the active grid.
    pub fn slot(&self, index: usize) -> Option<Item> {
        if index >= self.kind.slot_count() {
            return None;
        }
        self.slots.get(index).copied().flatten()
    }

    /// Sets a cell within the active grid. Items are already validated, so
    /// this cannot forge stack or durability state.
    pub fn set(&mut self, index: usize, item: Option<Item>) -> Result<(), CraftError> {
        if index >= self.kind.slot_count() {
            return Err(CraftError::OutOfBounds);
        }
        self.slots[index] = item;
        Ok(())
    }

    /// Takes a cell within the active grid, leaving it empty.
    pub fn take(&mut self, index: usize) -> Result<Option<Item>, CraftError> {
        if index >= self.kind.slot_count() {
            return Err(CraftError::OutOfBounds);
        }
        Ok(self.slots[index].take())
    }

    /// Rebuilds from decoded storage. Cells beyond the active region must be
    /// empty, which rejects malformed persisted grids.
    pub fn from_slots(
        kind: GridKind,
        slots: [Option<Item>; MAX_GRID_SLOTS],
    ) -> Result<Self, CraftError> {
        if slots
            .iter()
            .skip(kind.slot_count())
            .any(|slot| slot.is_some())
        {
            return Err(CraftError::InactiveCell);
        }
        Ok(Self { kind, slots })
    }

    pub fn is_empty(&self) -> bool {
        self.slots
            .iter()
            .take(self.kind.slot_count())
            .all(Option::is_none)
    }

    /// Evaluates the current grid contents against the built-in registry.
    /// Never cached: the result is a pure function of the cells at call time.
    pub fn preview(&self) -> Option<RecipePreview> {
        self.preview_with(ContentRegistry::builtin())
    }

    /// Evaluates the current grid contents against `registry`.
    ///
    /// Disabled and placeholder recipes are skipped, and every key character
    /// and the output resolve through the registry. Never caches.
    pub fn preview_with(&self, registry: &ContentRegistry) -> Option<RecipePreview> {
        recipe_for(self, registry).and_then(|recipe| {
            Some(RecipePreview {
                recipe: recipe_id_for_index(recipe.index),
                output: registry.resolve_item_ref(recipe.output)?,
                output_count: recipe.count,
            })
        })
    }

    /// Crafts one result against the built-in registry.
    ///
    /// Delegates to [`CraftingGrid::take_result_with`], so built-in recipes
    /// answer byte-identically to the registry-aware path.
    pub fn take_result(&mut self, inventory: &mut Inventory) -> Result<Item, CraftError> {
        self.take_result_with(ContentRegistry::builtin(), inventory)
    }

    /// Crafts one result against `registry`: revalidate the recipe and
    /// destination, insert the output, then consume exactly one item from each
    /// occupied cell.
    ///
    /// Any error (no recipe, full destination) leaves both the grid and the
    /// inventory untouched. The recipe is always re-derived from the current
    /// cells, so a stale caller-held preview can never take a result. A pack
    /// tool output is built through [`Item::new_with`], so it becomes
    /// [`ItemId::ModTool`] with authored durability.
    pub fn take_result_with(
        &mut self,
        registry: &ContentRegistry,
        inventory: &mut Inventory,
    ) -> Result<Item, CraftError> {
        let recipe = recipe_for(self, registry).ok_or(CraftError::NoRecipe)?;
        let output_id = registry
            .resolve_item_ref(recipe.output)
            .ok_or(CraftError::NoRecipe)?;
        let output =
            Item::new_with(registry, output_id, recipe.count).map_err(|_| CraftError::NoRecipe)?;
        if !inventory.can_insert(&output) {
            return Err(CraftError::DestinationFull);
        }

        // Destination capacity is proven above, so insertion cannot fail.
        inventory
            .insert(output)
            .map_err(|_| CraftError::DestinationFull)?;

        // Every occupied cell in a matched recipe is exactly one ingredient
        // unit; decrement once per cell and clear exhausted cells.
        for cell in self.slots.iter_mut().take(self.kind.slot_count()) {
            if let Some(item) = cell {
                let remaining = item.count - 1;
                *cell = if remaining == 0 {
                    None
                } else {
                    Some(
                        item.with_count(remaining)
                            .expect("recipe cells hold stackable ingredients"),
                    )
                };
            }
        }

        Ok(output)
    }
}

impl Default for CraftingGrid {
    fn default() -> Self {
        Self::new(GridKind::Player)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: ItemId, count: u16) -> Item {
        Item::new(id, count).expect("valid test item")
    }

    fn block(block: BlockId, count: u16) -> Item {
        item(ItemId::Block(block), count)
    }

    fn stick(count: u16) -> Item {
        item(ItemId::Material(Material::Stick), count)
    }

    fn wheat(count: u16) -> Item {
        item(ItemId::Material(Material::Wheat), count)
    }

    fn tool(kind: ToolKind, tier: ToolTier) -> Item {
        item(ItemId::Tool { kind, tier }, 1)
    }

    /// A deterministic pack registry with one tier (durability 900) and one
    /// tool pointing at it, built through the public registry builder so the
    /// test exercises the same allocation path as a real pack.
    fn fixture_registry() -> crate::content::ContentRegistry {
        use crate::content::build_registry;
        use crate::content::spec::{PackAsset, PackContentSpec, TierEntry, ToolEntry};
        use crate::content::PackIdentity;

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
                rgba: vec![0u8; 16 * 16 * 4],
                source_side: 16,
            },
        );
        let (registry, report) = build_registry(
            crate::content::ContentRegistry::builtin(),
            &[(
                PackIdentity {
                    folder: "fixture".to_string(),
                    version: None,
                },
                spec,
            )],
        );
        assert_eq!(report.errors(), 0, "fixture registry must not error");
        registry
    }

    /// A pack registry with one generic item, one tier/tool and two shaped
    /// recipes: a 2x2 gem recipe (from stone) and a 4x4 ruby pickaxe recipe
    /// (gems + sticks). Recipe ids sort as `gem_from_stone` then
    /// `ruby_pickaxe`, allocating dense indices 22 and 23.
    fn recipe_fixture_registry() -> crate::content::ContentRegistry {
        use crate::content::build_registry;
        use crate::content::spec::{
            ItemEntry, PackAsset, PackContentSpec, RecipeEntry, TierEntry, ToolEntry,
        };
        use crate::content::PackIdentity;
        use std::collections::BTreeMap;

        let mut spec = PackContentSpec {
            items: vec![ItemEntry {
                id: "mymod:gem".to_string(),
                display_name: Some("Ruby Gem".to_string()),
                icon: "gem.png".to_string(),
                stack_limit: 12,
                food: None,
            }],
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
                icon: "pickaxe.png".to_string(),
            }],
            recipes: vec![
                RecipeEntry {
                    id: "mymod:gem_from_stone".to_string(),
                    pattern: vec!["SS".to_string(), "SS".to_string()],
                    key: [('S', "stone".to_string())]
                        .into_iter()
                        .collect::<BTreeMap<_, _>>(),
                    output: "mymod:gem".to_string(),
                    count: 4,
                },
                RecipeEntry {
                    id: "mymod:ruby_pickaxe".to_string(),
                    pattern: vec!["RRR".to_string(), " S ".to_string(), " S ".to_string()],
                    key: [('R', "mymod:gem".to_string()), ('S', "stick".to_string())]
                        .into_iter()
                        .collect::<BTreeMap<_, _>>(),
                    output: "mymod:ruby_pickaxe".to_string(),
                    count: 1,
                },
            ],
            ..PackContentSpec::default()
        };
        for icon in ["gem.png", "pickaxe.png"] {
            spec.assets.insert(
                icon.to_string(),
                PackAsset {
                    rgba: vec![0u8; 16 * 16 * 4],
                    source_side: 16,
                },
            );
        }
        let (registry, report) = build_registry(
            crate::content::ContentRegistry::builtin(),
            &[(
                PackIdentity {
                    folder: "fixture".to_string(),
                    version: None,
                },
                spec,
            )],
        );
        assert_eq!(report.errors(), 0, "recipe fixture must not error");
        registry
    }

    fn all_carryable_builtin_blocks() -> Vec<BlockId> {
        (0..crate::world::block::BUILTIN_BLOCK_COUNT)
            .map(BlockId::from_raw)
            .filter(|block| block.is_carryable())
            .collect()
    }

    fn all_materials() -> [Material; 11] {
        [
            Material::Stick,
            Material::Apple,
            Material::Wheat,
            Material::WheatSeeds,
            Material::Bread,
            Material::Leather,
            Material::RawPorkchop,
            Material::RawBeef,
            Material::RottenFlesh,
            Material::Bone,
            Material::Arrow,
        ]
    }

    fn all_constructible_tools() -> Vec<ItemId> {
        let mut ids = Vec::new();
        for kind in [
            ToolKind::Pickaxe,
            ToolKind::Axe,
            ToolKind::Shovel,
            ToolKind::Hoe,
            ToolKind::Sword,
            ToolKind::Bow,
        ] {
            for &tier in kind.allowed_tiers() {
                ids.push(ItemId::Tool { kind, tier });
            }
        }
        ids
    }

    fn total_of(inventory: &Inventory, id: ItemId) -> u32 {
        inventory
            .slots()
            .iter()
            .flatten()
            .chain(inventory.cursor().iter())
            .filter(|item| item.id() == id)
            .map(|item| u32::from(item.count()))
            .sum()
    }

    fn fill_with(inventory: &mut Inventory, id: ItemId) {
        let full = Item::new(id, id.stack_limit()).expect("full stack is valid");
        for slot in inventory.slots.iter_mut() {
            *slot = Some(full);
        }
    }

    /// Removes `amount` of `id` from the inventory, failing the test if the
    /// inventory cannot supply it. Keeps craft-chain accounting honest: every
    /// unit placed in a grid is withdrawn from somewhere.
    fn withdraw(inventory: &mut Inventory, id: ItemId, amount: u16) -> Item {
        let mut remaining = amount;
        for slot in inventory.slots.iter_mut() {
            if remaining == 0 {
                break;
            }
            if let Some(existing) = *slot {
                if existing.id() != id {
                    continue;
                }
                let moved = existing.count.min(remaining);
                remaining -= moved;
                *slot = if existing.count == moved {
                    None
                } else {
                    Some(
                        existing
                            .with_count(existing.count - moved)
                            .expect("partial stack is valid"),
                    )
                };
            }
        }
        assert_eq!(remaining, 0, "inventory holds the requested ingredient");
        item(id, amount)
    }

    /// Builds a grid of `kind` from an ASCII pattern using the same character
    /// map the recipe table uses.
    fn grid_from_pattern(kind: GridKind, pattern: &[&str]) -> CraftingGrid {
        let mut grid = CraftingGrid::new(kind);
        for (row, line) in pattern.iter().enumerate() {
            for (col, byte) in line.bytes().enumerate() {
                if byte == b' ' {
                    continue;
                }
                let id = pattern_item(byte).expect("pattern character known");
                grid.set(row * kind.dim() + col, Some(item(id, 1)))
                    .expect("pattern fits grid");
            }
        }
        grid
    }

    // -- Legacy block-stack / hotbar compatibility ---------------------------

    #[test]
    fn item_stack_rejects_invalid_blocks_and_counts() {
        assert!(ItemStack::new(BlockId::Air, 1).is_none());
        assert!(ItemStack::new(BlockId::Bedrock, 1).is_none());
        assert!(ItemStack::new(BlockId::Water, 1).is_none());
        assert!(ItemStack::new(BlockId::Grass, 0).is_none());
        assert!(ItemStack::new(BlockId::Grass, MAX_STACK_COUNT + 1).is_none());

        for block in [BlockId::Grass, BlockId::CoalOre, BlockId::IronOre] {
            let one = ItemStack::new(block, 1).expect("count 1 is valid");
            assert_eq!(one.block, block);
            assert_eq!(one.count, 1);

            let full = ItemStack::new(block, MAX_STACK_COUNT).expect("max count is valid");
            assert_eq!(full.count, MAX_STACK_COUNT);
        }
    }

    #[test]
    fn empty_hotbar_has_nine_empty_slots() {
        let hotbar = Hotbar::empty();
        assert_eq!(hotbar.stacks().len(), HOTBAR_SLOTS);
        assert_eq!(hotbar.stacks().iter().filter(|s| s.is_none()).count(), 9);
        assert_eq!(hotbar, Hotbar::default());
    }

    #[test]
    fn stack_is_bounds_checked() {
        let hotbar = Hotbar::empty();
        assert_eq!(hotbar.stack(0), None);
        assert_eq!(hotbar.stack(HOTBAR_SLOTS - 1), None);
        assert_eq!(hotbar.stack(HOTBAR_SLOTS), None);
        assert_eq!(hotbar.stack(999), None);
    }

    #[test]
    fn collect_puts_first_stack_in_slot_zero_and_merges_matching_stacks() {
        let mut hotbar = Hotbar::empty();

        assert_eq!(hotbar.collect(BlockId::CoalOre, 3), Some(0));
        assert_eq!(hotbar.stack(0), ItemStack::new(BlockId::CoalOre, 3));

        assert_eq!(hotbar.collect(BlockId::CoalOre, 2), Some(0));
        assert_eq!(hotbar.stack(0), ItemStack::new(BlockId::CoalOre, 5));
        assert_eq!(hotbar.stacks().iter().filter(|s| s.is_some()).count(), 1);
    }

    #[test]
    fn collect_uses_first_empty_slot_for_a_different_block() {
        let mut hotbar = Hotbar::empty();
        hotbar.collect(BlockId::CoalOre, 3).unwrap();

        assert_eq!(hotbar.collect(BlockId::IronOre, 4), Some(1));
        assert_eq!(hotbar.stack(0), ItemStack::new(BlockId::CoalOre, 3));
        assert_eq!(hotbar.stack(1), ItemStack::new(BlockId::IronOre, 4));

        // Same block returns to the first non-full matching slot.
        assert_eq!(hotbar.collect(BlockId::CoalOre, 1), Some(0));
        assert_eq!(hotbar.stack(0), ItemStack::new(BlockId::CoalOre, 4));
    }

    #[test]
    fn collect_refills_first_non_full_matching_slot_after_from_slots() {
        let mut hotbar = Hotbar::from_slots([
            Some(ItemStack::new(BlockId::CoalOre, 2).unwrap()),
            Some(ItemStack::new(BlockId::Stone, 1).unwrap()),
            Some(ItemStack::new(BlockId::CoalOre, 5).unwrap()),
            None,
            None,
            None,
            None,
            None,
            None,
        ]);

        assert_eq!(hotbar.collect(BlockId::CoalOre, 1), Some(0));
        assert_eq!(hotbar.stack(0).unwrap().count, 3);
        assert_eq!(hotbar.stack(2).unwrap().count, 5);
    }

    #[test]
    fn full_stack_forces_a_second_slot() {
        let mut hotbar = Hotbar::empty();
        assert_eq!(hotbar.collect(BlockId::CoalOre, MAX_STACK_COUNT), Some(0));
        assert_eq!(hotbar.stack(0).unwrap().count, MAX_STACK_COUNT);

        assert_eq!(hotbar.collect(BlockId::CoalOre, 1), Some(1));
        assert_eq!(hotbar.stack(0).unwrap().count, MAX_STACK_COUNT);
        assert_eq!(hotbar.stack(1), ItemStack::new(BlockId::CoalOre, 1));
    }

    #[test]
    fn collect_splits_overflow_across_slots_without_wrapping() {
        let mut hotbar = Hotbar::empty();
        let amount = MAX_STACK_COUNT + 501;

        assert_eq!(hotbar.collect(BlockId::IronOre, amount), Some(0));
        assert_eq!(hotbar.stack(0).unwrap().block, BlockId::IronOre);
        assert_eq!(hotbar.stack(0).unwrap().count, MAX_STACK_COUNT);
        assert_eq!(hotbar.stack(1).unwrap().block, BlockId::IronOre);
        assert_eq!(hotbar.stack(1).unwrap().count, 501);

        let total: u32 = hotbar
            .stacks()
            .iter()
            .flatten()
            .map(|s| u32::from(s.count))
            .sum();
        assert_eq!(total, u32::from(amount));
        assert_eq!(hotbar.stacks().iter().filter(|s| s.is_some()).count(), 2);
    }

    #[test]
    fn collect_continues_into_empty_slots_after_topping_up_a_partial_stack() {
        let mut hotbar = Hotbar::from_slots([
            Some(ItemStack::new(BlockId::Stone, MAX_STACK_COUNT - 2).unwrap()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ]);

        assert_eq!(hotbar.collect(BlockId::Stone, 10), Some(0));
        assert_eq!(hotbar.stack(0).unwrap().count, MAX_STACK_COUNT);
        assert_eq!(hotbar.stack(1), ItemStack::new(BlockId::Stone, 8));
    }

    #[test]
    fn collect_returns_none_when_no_slot_can_accept_and_does_not_mutate() {
        let full = ItemStack::new(BlockId::CoalOre, MAX_STACK_COUNT).unwrap();
        let mut hotbar = Hotbar::from_slots([Some(full); HOTBAR_SLOTS]);

        assert_eq!(hotbar.collect(BlockId::IronOre, 1), None);
        assert_eq!(hotbar.stacks(), &[Some(full); HOTBAR_SLOTS]);
    }

    #[test]
    fn collect_rejects_invalid_blocks_and_amount_without_mutating() {
        let mut hotbar = Hotbar::empty();

        assert_eq!(hotbar.collect(BlockId::Air, 1), None);
        assert_eq!(hotbar.collect(BlockId::Bedrock, 1), None);
        assert_eq!(hotbar.collect(BlockId::Water, 1), None);
        assert_eq!(hotbar.collect(BlockId::Grass, 0), None);
        assert_eq!(hotbar, Hotbar::empty());
    }

    #[test]
    fn consume_is_bounds_checked_and_handles_empty_slots() {
        let mut hotbar = Hotbar::empty();
        assert_eq!(hotbar.consume(0), None);
        assert_eq!(hotbar.consume(HOTBAR_SLOTS), None);
        assert_eq!(hotbar.consume(999), None);
    }

    #[test]
    fn consume_decrements_and_clears_the_last_item() {
        let mut hotbar = Hotbar::from_slots([
            Some(ItemStack::new(BlockId::CoalOre, 2).unwrap()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ]);

        assert_eq!(hotbar.consume(0), Some(BlockId::CoalOre));
        assert_eq!(hotbar.stack(0), ItemStack::new(BlockId::CoalOre, 1));

        assert_eq!(hotbar.consume(0), Some(BlockId::CoalOre));
        assert_eq!(hotbar.stack(0), None);
        assert_eq!(hotbar.consume(0), None);
    }

    // -- Item validation ----------------------------------------------------

    #[test]
    fn item_rejects_uncarryable_blocks_and_bad_counts() {
        assert_eq!(
            Item::new(ItemId::Block(BlockId::Air), 1),
            Err(ItemError::UncarryableBlock)
        );
        assert_eq!(
            Item::new(ItemId::Block(BlockId::Bedrock), 1),
            Err(ItemError::UncarryableBlock)
        );
        assert_eq!(
            Item::new(ItemId::Block(BlockId::Water), 1),
            Err(ItemError::UncarryableBlock)
        );
        assert_eq!(
            Item::new(ItemId::Block(BlockId::Water), MAX_STACK_COUNT),
            Err(ItemError::UncarryableBlock),
            "the uncarryable check must win over the count check"
        );
        assert_eq!(
            Item::new(ItemId::Block(BlockId::Stone), 0),
            Err(ItemError::EmptyStack)
        );
        assert_eq!(
            Item::new(ItemId::Block(BlockId::Stone), MAX_STACK_COUNT + 1),
            Err(ItemError::StackOverflow)
        );
        assert_eq!(
            Item::new(ItemId::Material(Material::Stick), 0),
            Err(ItemError::EmptyStack)
        );
        assert_eq!(
            Item::new(ItemId::Material(Material::Stick), MATERIAL_STACK_LIMIT + 1),
            Err(ItemError::StackOverflow)
        );

        assert_eq!(ItemId::Block(BlockId::Stone).stack_limit(), MAX_STACK_COUNT);
        assert_eq!(
            ItemId::Material(Material::Stick).stack_limit(),
            MATERIAL_STACK_LIMIT
        );
        assert_eq!(
            ItemId::Tool {
                kind: ToolKind::Pickaxe,
                tier: ToolTier::Iron
            }
            .stack_limit(),
            1
        );
    }

    #[test]
    fn central_carryability_predicate_drives_all_item_constructors() {
        let uncarryable = [
            BlockId::Farmland,
            BlockId::WheatStage0,
            BlockId::WheatStage1,
            BlockId::WheatStage2,
            BlockId::WheatStage3,
            BlockId::LootChest,
        ];
        for block in uncarryable {
            assert!(
                ItemStack::new(block, 1).is_none(),
                "{block:?} must be rejected by the legacy stack constructor"
            );
            assert_eq!(
                Item::new(ItemId::Block(block), 1),
                Err(ItemError::UncarryableBlock),
                "{block:?} must be rejected by Item::new"
            );
            assert_eq!(
                Hotbar::empty().collect(block, 1),
                None,
                "{block:?} must be rejected by the legacy hotbar"
            );
            assert_eq!(
                Inventory::empty().collect_block(block, 1),
                None,
                "{block:?} must be rejected by Inventory::collect_block"
            );
        }

        // Chest is the one carryable block appended in S02 and must still flow
        // through every constructor.
        let stack = ItemStack::new(BlockId::Chest, 1).expect("chest stack");
        assert_eq!(stack.block, BlockId::Chest);
        let item = Item::new(ItemId::Block(BlockId::Chest), 1).expect("chest item");
        assert_eq!(item.id(), ItemId::Block(BlockId::Chest));

        let mut hotbar = Hotbar::empty();
        assert_eq!(hotbar.collect(BlockId::Chest, 1), Some(0));
        assert_eq!(
            hotbar.stack(0),
            Some(ItemStack {
                block: BlockId::Chest,
                count: 1
            })
        );

        let mut inventory = Inventory::empty();
        assert_eq!(
            inventory.collect_block(BlockId::Chest, 2),
            Some(CollectOutcome {
                first_slot: 0,
                accepted: 2
            })
        );
    }

    #[test]
    fn loot_vocabulary_maps_to_valid_item_identities() {
        use crate::world::loot::LootItem;

        let cases = [
            (LootItem::Apple, ItemId::Material(Material::Apple)),
            (LootItem::Bread, ItemId::Material(Material::Bread)),
            (LootItem::Wheat, ItemId::Material(Material::Wheat)),
            (LootItem::WheatSeeds, ItemId::Material(Material::WheatSeeds)),
            (LootItem::Stick, ItemId::Material(Material::Stick)),
            (LootItem::CoalOre, ItemId::Block(BlockId::CoalOre)),
            (LootItem::IronOre, ItemId::Block(BlockId::IronOre)),
        ];

        for (loot, expected) in cases {
            let id: ItemId = loot.into();
            assert_eq!(id, expected, "{loot:?} maps to the wrong identity");
            let item = Item::new(id, 1)
                .unwrap_or_else(|error| panic!("{loot:?} must build an item, got {error:?}"));
            assert_eq!(item.id(), expected);
            assert_eq!(item.count(), 1);
        }
    }

    #[test]
    fn mob_drop_vocabulary_maps_to_valid_item_identities() {
        use crate::world::mob_loot::MobDropItem;

        // Exhaustive on purpose: adding a `MobDropItem` variant must fail this
        // test (and the `From` match) until it has a pinned identity.
        let cases = [
            (MobDropItem::RawPorkchop, Material::RawPorkchop),
            (MobDropItem::RawBeef, Material::RawBeef),
            (MobDropItem::Leather, Material::Leather),
            (MobDropItem::RottenFlesh, Material::RottenFlesh),
            (MobDropItem::Bone, Material::Bone),
            (MobDropItem::Arrow, Material::Arrow),
        ];
        let every_variant = [
            MobDropItem::RawPorkchop,
            MobDropItem::RawBeef,
            MobDropItem::Leather,
            MobDropItem::RottenFlesh,
            MobDropItem::Bone,
            MobDropItem::Arrow,
            MobDropItem::Bow,
        ];
        assert_eq!(
            cases.len() + 1,
            every_variant.len(),
            "every MobDropItem variant must appear in the mapping table"
        );

        for (drop, material) in cases {
            let expected = ItemId::Material(material);
            let id: ItemId = drop.into();
            assert_eq!(id, expected, "{drop:?} maps to the wrong identity");
            assert_eq!(
                id.stack_limit(),
                MATERIAL_STACK_LIMIT,
                "{drop:?} must be a full material stack"
            );
            assert_eq!(MATERIAL_STACK_LIMIT, 64);
            let item = Item::new(id, 1)
                .unwrap_or_else(|error| panic!("{drop:?} must build an item, got {error:?}"));
            assert_eq!(item.id(), expected);
            assert_eq!(item.count(), 1);
        }

        // The bow is the one non-material drop: a wood-only tool, so the
        // identity is non-stackable and `Item::new` grants full durability.
        let bow: ItemId = MobDropItem::Bow.into();
        assert_eq!(
            bow,
            ItemId::Tool {
                kind: ToolKind::Bow,
                tier: ToolTier::Wood,
            }
        );
        assert_eq!(bow.stack_limit(), 1);
        let bow_item = Item::new(bow, 1).expect("a wood bow must be constructible");
        assert_eq!(bow_item.id(), bow);
        assert_eq!(bow_item.count(), 1);
        assert_eq!(
            bow_item.durability(),
            Some(ToolTier::Wood.max_durability()),
            "a dropped bow must enter at full wood-bow durability"
        );
    }

    #[test]
    fn widened_pack_item_identity_is_stackable_and_inert() {
        let id = ItemId::Item(0);
        assert_eq!(id.stack_limit(), MAX_STACK_COUNT);
        assert!(!id.is_tool());
        assert_eq!(id.tool(), None);
        assert_eq!(id.name(), "Modded Item");

        for count in [1, 2, MAX_STACK_COUNT] {
            let built = Item::new(id, count).expect("pack items stack like blocks");
            assert_eq!(built.id(), id);
            assert_eq!(built.count(), count);
            assert_eq!(built.durability(), None);
            assert!(!built.is_tool());
            assert!(built.is_stackable());
            assert_eq!(built.block_stack(), None);
            assert_eq!(built.max_durability(), None);
        }
        assert_eq!(Item::new(id, 0), Err(ItemError::EmptyStack));
        assert_eq!(
            Item::new(id, MAX_STACK_COUNT + 1),
            Err(ItemError::StackOverflow)
        );

        let stack = Item::new(id, 2).expect("valid pack stack");
        assert!(stack.mergeable_with(&stack));
        assert_ne!(stack.id(), ItemId::Item(1));
    }

    #[test]
    fn item_identities_have_stable_names_and_limits() {
        assert_eq!(Material::Stick.name(), "Stick");
        assert_eq!(Material::Apple.name(), "Apple");
        assert_eq!(Material::Wheat.name(), "Wheat");
        assert_eq!(Material::WheatSeeds.name(), "Wheat Seeds");
        assert_eq!(Material::Bread.name(), "Bread");
        assert_eq!(Material::Leather.name(), "Leather");
        assert_eq!(Material::RawPorkchop.name(), "Raw Porkchop");
        assert_eq!(Material::RawBeef.name(), "Raw Beef");
        assert_eq!(Material::RottenFlesh.name(), "Rotten Flesh");
        assert_eq!(Material::Bone.name(), "Bone");
        assert_eq!(Material::Arrow.name(), "Arrow");
        assert_eq!(ToolKind::Hoe.name(), "Hoe");
        assert_eq!(ToolKind::Sword.name(), "Sword");
        assert_eq!(ToolKind::Bow.name(), "Bow");

        for material in [
            Material::Apple,
            Material::Wheat,
            Material::WheatSeeds,
            Material::Bread,
            Material::Leather,
            Material::RawPorkchop,
            Material::RawBeef,
            Material::RottenFlesh,
            Material::Bone,
            Material::Arrow,
        ] {
            let id = ItemId::Material(material);
            assert_eq!(id.name(), material.name());
            assert_eq!(id.stack_limit(), MATERIAL_STACK_LIMIT);
            assert_eq!(Item::new(id, 0), Err(ItemError::EmptyStack));
            assert_eq!(
                Item::new(id, MATERIAL_STACK_LIMIT + 1),
                Err(ItemError::StackOverflow)
            );
            assert!(Item::new(id, MATERIAL_STACK_LIMIT).is_ok());
            assert!(item(id, MATERIAL_STACK_LIMIT).is_stackable());
        }

        let names = [
            (ToolTier::Wood, "Wood Hoe"),
            (ToolTier::Stone, "Stone Hoe"),
            (ToolTier::Iron, "Iron Hoe"),
        ];
        for (tier, name) in names {
            let id = ItemId::Tool {
                kind: ToolKind::Hoe,
                tier,
            };
            assert_eq!(id.name(), name);
            assert_eq!(id.stack_limit(), 1);
            let fresh = item(id, 1);
            assert!(fresh.is_tool());
            assert!(!fresh.is_stackable());
            assert_eq!(fresh.durability(), Some(tier.max_durability()));
            assert_eq!(fresh.max_durability(), Some(tier.max_durability()));

            // Hoes reuse the checked durability constructor, so they inherit
            // the same zero/over-maximum rejection as every other tool.
            assert_eq!(
                Item::tool_with_durability(ToolKind::Hoe, tier, 0),
                Err(ItemError::InvalidDurability)
            );
            assert_eq!(
                Item::tool_with_durability(ToolKind::Hoe, tier, tier.max_durability() + 1),
                Err(ItemError::InvalidDurability)
            );
            assert_eq!(
                Item::tool_with_durability(ToolKind::Hoe, tier, tier.max_durability())
                    .expect("full durability is valid")
                    .durability(),
                Some(tier.max_durability())
            );
        }

        for (tier, name) in [
            (ToolTier::Wood, "Wood Sword"),
            (ToolTier::Stone, "Stone Sword"),
            (ToolTier::Iron, "Iron Sword"),
        ] {
            let id = ItemId::Tool {
                kind: ToolKind::Sword,
                tier,
            };
            assert_eq!(id.name(), name);
            assert_eq!(id.stack_limit(), 1);
            let fresh = item(id, 1);
            assert!(!fresh.is_stackable());
            assert_eq!(fresh.durability(), Some(tier.max_durability()));
        }

        let bow = ItemId::Tool {
            kind: ToolKind::Bow,
            tier: ToolTier::Wood,
        };
        assert_eq!(ToolKind::Bow.allowed_tiers(), &[ToolTier::Wood]);
        assert_eq!(bow.name(), "Wood Bow");
        assert_eq!(bow.stack_limit(), 1);
        assert!(!item(bow, 1).is_stackable());

        // Forged (bow, stone/iron) records are unrepresentable in both
        // constructors and surface as TierNotAllowed, never InvalidDurability.
        for tier in [ToolTier::Stone, ToolTier::Iron] {
            assert_eq!(
                Item::new(
                    ItemId::Tool {
                        kind: ToolKind::Bow,
                        tier
                    },
                    1
                ),
                Err(ItemError::TierNotAllowed)
            );
            assert_eq!(
                Item::tool_with_durability(ToolKind::Bow, tier, tier.max_durability()),
                Err(ItemError::TierNotAllowed)
            );
        }

        assert_eq!(CHEST_SLOTS, 27);
        let chest: ChestSlots = [None; CHEST_SLOTS];
        assert_eq!(chest.len(), 27, "a chest carries exactly 27 slots");
    }

    #[test]
    fn tools_are_non_stackable_and_start_at_full_durability() {
        assert_eq!(
            Item::new(
                ItemId::Tool {
                    kind: ToolKind::Pickaxe,
                    tier: ToolTier::Wood
                },
                2
            ),
            Err(ItemError::StackableTool)
        );

        for tier in [ToolTier::Wood, ToolTier::Stone, ToolTier::Iron] {
            let pickaxe = tool(ToolKind::Pickaxe, tier);
            assert!(!pickaxe.is_stackable());
            assert!(pickaxe.is_tool());
            assert_eq!(pickaxe.count(), 1);
            assert_eq!(pickaxe.durability(), Some(tier.max_durability()));
            assert_eq!(pickaxe.max_durability(), Some(tier.max_durability()));
        }

        assert!(block(BlockId::Stone, 1).is_stackable());
    }

    #[test]
    fn tool_durability_constructor_rejects_impossible_values() {
        assert_eq!(
            Item::tool_with_durability(ToolKind::Pickaxe, ToolTier::Iron, 0),
            Err(ItemError::InvalidDurability)
        );
        assert_eq!(
            Item::tool_with_durability(
                ToolKind::Pickaxe,
                ToolTier::Iron,
                ToolTier::Iron.max_durability() + 1
            ),
            Err(ItemError::InvalidDurability)
        );

        let worn = Item::tool_with_durability(ToolKind::Axe, ToolTier::Stone, 7)
            .expect("mid-range durability is valid");
        assert_eq!(worn.durability(), Some(7));
        assert_eq!(worn.count(), 1);
        assert_eq!(
            worn.id(),
            ItemId::Tool {
                kind: ToolKind::Axe,
                tier: ToolTier::Stone
            }
        );
    }

    #[test]
    fn new_with_matches_new_for_every_builtin_identity() {
        let registry = ContentRegistry::builtin();

        for block in all_carryable_builtin_blocks() {
            let id = ItemId::Block(block);
            for count in [1, 2, MAX_STACK_COUNT] {
                assert_eq!(
                    Item::new_with(registry, id, count),
                    Item::new(id, count),
                    "block {block:?} count {count} must answer identically"
                );
            }
            assert_eq!(
                Item::new_with(registry, id, MAX_STACK_COUNT + 1),
                Item::new(id, MAX_STACK_COUNT + 1)
            );
            assert_eq!(
                Item::new_with(registry, id, 0),
                Item::new(id, 0),
                "zero counts stay EmptyStack"
            );
        }

        for material in all_materials() {
            let id = ItemId::Material(material);
            for count in [1, MATERIAL_STACK_LIMIT] {
                assert_eq!(
                    Item::new_with(registry, id, count),
                    Item::new(id, count),
                    "material {material:?} count {count} must answer identically"
                );
            }
            assert_eq!(
                Item::new_with(registry, id, MATERIAL_STACK_LIMIT + 1),
                Item::new(id, MATERIAL_STACK_LIMIT + 1)
            );
        }

        for id in all_constructible_tools() {
            assert_eq!(
                Item::new_with(registry, id, 1),
                Item::new(id, 1),
                "tool {id:?} must answer identically"
            );
            assert_eq!(
                Item::new_with(registry, id, 2),
                Item::new(id, 2),
                "tool stack rejection must match"
            );
        }

        // Uncarryable blocks and unconstructible tool pairs stay rejected.
        assert_eq!(
            Item::new_with(registry, ItemId::Block(BlockId::Water), 1),
            Err(ItemError::UncarryableBlock)
        );
        assert_eq!(
            Item::new_with(
                registry,
                ItemId::Tool {
                    kind: ToolKind::Bow,
                    tier: ToolTier::Iron
                },
                1
            ),
            Err(ItemError::TierNotAllowed)
        );
    }

    #[test]
    fn mod_tool_item_carries_full_authored_durability() {
        let registry = fixture_registry();
        assert_eq!(
            registry.tool(0).map(|def| def.name.as_str()),
            Some("mymod:ruby_pickaxe")
        );
        assert_eq!(registry.tier(3).map(|def| def.durability), Some(900));

        let id = ItemId::ModTool(0);
        assert_eq!(id.stack_limit(), 1);
        assert!(id.is_tool());
        assert_eq!(id.tool(), None);
        assert_eq!(id.name(), "Modded Tool");

        let fresh = Item::new_with(&registry, id, 1).expect("pack tool is constructible");
        assert_eq!(fresh.id(), id);
        assert_eq!(fresh.count(), 1);
        assert_eq!(fresh.durability(), Some(900));
        assert_eq!(fresh.max_durability(), Some(900));
        assert!(fresh.is_tool());
        assert!(!fresh.is_stackable());
        assert_eq!(
            fresh.use_tool_once(),
            Ok(Some(
                Item::tool_with_durability_with(&registry, id, 899).expect("899 is in range")
            ))
        );

        // The built-in constructors cannot express a pack tool.
        assert_eq!(Item::new(id, 1), Err(ItemError::RegistryRequired));
    }

    #[test]
    fn new_with_rejects_unknown_content_and_bad_counts() {
        let registry = fixture_registry();
        let id = ItemId::ModTool(0);

        assert_eq!(
            Item::new_with(&registry, id, 2),
            Err(ItemError::StackableTool)
        );
        assert_eq!(
            Item::new_with(&registry, id, 0),
            Err(ItemError::StackableTool)
        );
        assert_eq!(
            Item::new_with(&registry, ItemId::ModTool(99), 1),
            Err(ItemError::UnknownContent)
        );
        assert_eq!(
            Item::new_with(&registry, ItemId::Item(0), 1),
            Err(ItemError::UnknownContent)
        );
    }

    #[test]
    fn tool_with_durability_with_accepts_authored_range_only() {
        let registry = fixture_registry();
        let id = ItemId::ModTool(0);

        assert_eq!(
            Item::tool_with_durability_with(&registry, id, 0),
            Err(ItemError::InvalidDurability)
        );
        assert_eq!(
            Item::tool_with_durability_with(&registry, id, 901),
            Err(ItemError::InvalidDurability)
        );
        for durability in [1, 900] {
            let item = Item::tool_with_durability_with(&registry, id, durability)
                .expect("authored durability is valid");
            assert_eq!(item.durability(), Some(durability));
            assert_eq!(item.max_durability(), Some(900));
        }
        assert_eq!(
            Item::tool_with_durability_with(&registry, ItemId::ModTool(99), 1),
            Err(ItemError::UnknownContent)
        );
        assert_eq!(
            Item::tool_with_durability_with(&registry, ItemId::Material(Material::Stick), 1),
            Err(ItemError::NotATool)
        );
        // Built-in ids delegate to the strict built-in rule.
        assert_eq!(
            Item::tool_with_durability_with(
                &registry,
                ItemId::Tool {
                    kind: ToolKind::Pickaxe,
                    tier: ToolTier::Iron
                },
                ToolTier::Iron.max_durability() + 1
            ),
            Err(ItemError::InvalidDurability)
        );
    }

    #[test]
    fn use_tool_once_decrements_and_reports_breakage() {
        let worn = Item::tool_with_durability(ToolKind::Shovel, ToolTier::Wood, 2)
            .expect("durability 2 is valid");
        let once = worn
            .use_tool_once()
            .expect("tool wear succeeds")
            .expect("still usable");
        assert_eq!(once.durability(), Some(1));

        assert_eq!(once.use_tool_once(), Ok(None));
        assert_eq!(
            block(BlockId::Stone, 1).use_tool_once(),
            Err(ItemError::NotATool)
        );
    }

    // -- Inventory shape and insertion --------------------------------------

    #[test]
    fn inventory_layout_has_nine_hotbar_and_twenty_seven_main_slots() {
        assert_eq!(INVENTORY_SLOTS, 36);
        let inventory = Inventory::empty();
        assert_eq!(inventory.slots().len(), INVENTORY_SLOTS);
        assert!(inventory.slots().iter().all(Option::is_none));
        assert_eq!(inventory.cursor(), None);

        for slot in 0..HOTBAR_SLOTS {
            assert_eq!(inventory.hotbar(slot), None);
        }
        for slot in 0..MAIN_SLOTS {
            assert_eq!(inventory.main(slot), None);
        }

        assert_eq!(inventory.hotbar(HOTBAR_SLOTS), None);
        assert_eq!(inventory.main(MAIN_SLOTS), None);
        assert_eq!(inventory.slot(INVENTORY_SLOTS - 1), None);
        assert_eq!(inventory.slot(INVENTORY_SLOTS), None);
    }

    #[test]
    fn insert_tops_up_then_spills_hotbar_first_then_main() {
        let mut inventory = Inventory::empty();
        inventory
            .insert(block(BlockId::Stone, MAX_STACK_COUNT - 10))
            .expect("first stack fits");

        inventory
            .insert(block(BlockId::Stone, 25))
            .expect("merge plus spill fits");

        assert_eq!(
            inventory.hotbar(0).map(|item| item.count()),
            Some(MAX_STACK_COUNT)
        );
        assert_eq!(inventory.hotbar(1).map(|item| item.count()), Some(15));
        assert_eq!(
            total_of(&inventory, ItemId::Block(BlockId::Stone)),
            u32::from(MAX_STACK_COUNT) + 15
        );

        // Filling 9 hotbar slots then spilling proves the main region is
        // addressable in flat order.
        let mut inventory = Inventory::empty();
        for _ in 0..HOTBAR_SLOTS {
            inventory
                .insert(block(BlockId::Dirt, MAX_STACK_COUNT))
                .expect("hotbar stack fits");
        }
        inventory
            .insert(block(BlockId::Dirt, 3))
            .expect("spills into main slot 0");
        assert_eq!(inventory.main(0).map(|item| item.count()), Some(3));
        assert_eq!(
            inventory.hotbar(0).map(|item| item.count()),
            Some(MAX_STACK_COUNT)
        );
    }

    #[test]
    fn insert_is_all_or_nothing_when_full() {
        let mut inventory = Inventory::empty();
        fill_with(&mut inventory, ItemId::Block(BlockId::Brick));
        let before = inventory.clone();

        assert_eq!(
            inventory.insert(block(BlockId::IronOre, 1)),
            Err(InventoryError::NoRoom)
        );
        assert_eq!(inventory, before);
    }

    #[test]
    fn insert_never_merges_non_stackable_tools() {
        let mut inventory = Inventory::empty();
        let pickaxe = tool(ToolKind::Pickaxe, ToolTier::Iron);
        inventory.insert(pickaxe).expect("first tool fits");
        inventory
            .insert(pickaxe)
            .expect("second tool takes its own slot");

        assert_eq!(inventory.hotbar(0).map(|item| item.count()), Some(1));
        assert_eq!(inventory.hotbar(1).map(|item| item.count()), Some(1));
        assert!(inventory.hotbar(0).unwrap().is_tool());
    }

    #[test]
    fn insert_all_inserts_every_item_and_leaves_the_cursor_alone() {
        let mut inventory = Inventory::empty();
        inventory.slots[0] = Some(block(BlockId::Stone, 900));
        inventory.cursor = Some(stick(7));

        let items = [block(BlockId::Stone, 50), stick(3)];
        let before = inventory.clone();
        assert!(inventory.can_insert_all(&items));
        assert_eq!(inventory, before, "a trial capacity check never mutates");

        inventory.insert_all(&items).expect("both items fit");
        assert_eq!(inventory.hotbar(0).map(|item| item.count()), Some(950));
        assert_eq!(inventory.hotbar(1), Some(stick(3)));
        assert_eq!(
            inventory.cursor(),
            Some(stick(7)),
            "the cursor is not a batch destination"
        );
        assert_eq!(total_of(&inventory, ItemId::Block(BlockId::Stone)), 950);
        assert_eq!(total_of(&inventory, ItemId::Material(Material::Stick)), 10);
    }

    #[test]
    fn insert_all_failure_preserves_every_slot_and_the_cursor() {
        let mut slots = [None; INVENTORY_SLOTS];
        let pickaxe = tool(ToolKind::Pickaxe, ToolTier::Iron);
        for slot in slots.iter_mut().take(INVENTORY_SLOTS - 1) {
            *slot = Some(pickaxe);
        }
        let mut inventory = Inventory::from_parts(slots, Some(stick(9)));
        let before = inventory.clone();

        // The stone would fill the single empty slot, then the axe has nowhere
        // to go; the batch must not commit the stone partially.
        let items = [
            block(BlockId::Stone, 1),
            tool(ToolKind::Axe, ToolTier::Iron),
        ];
        assert!(!inventory.can_insert_all(&items));
        assert_eq!(inventory.insert_all(&items), Err(InventoryError::NoRoom));
        assert_eq!(inventory, before, "a failed batch must not commit");
        assert_eq!(inventory.slot(INVENTORY_SLOTS - 1), None);
        assert_eq!(inventory.cursor(), Some(stick(9)));
    }

    #[test]
    fn cursor_contents_do_not_create_spill_capacity() {
        let mut inventory = Inventory::empty();
        fill_with(&mut inventory, ItemId::Block(BlockId::Brick));
        inventory.cursor = Some(block(BlockId::Stone, 1));
        let before = inventory.clone();

        let stone = [block(BlockId::Stone, 1)];
        assert!(
            !inventory.can_insert_all(&stone),
            "a matching cursor stack is not 36-slot capacity"
        );
        assert_eq!(inventory.insert_all(&stone), Err(InventoryError::NoRoom));
        assert_eq!(inventory, before);
    }

    #[test]
    fn insert_all_accepts_an_empty_batch_without_mutation() {
        let mut inventory = Inventory::empty();
        inventory.cursor = Some(stick(2));
        let before = inventory.clone();

        assert!(inventory.can_insert_all(&[]));
        assert_eq!(inventory.insert_all(&[]), Ok(()));
        assert_eq!(inventory, before);
    }

    #[test]
    fn collect_block_reports_first_slot_accepted_and_conserves_items() {
        let mut inventory = Inventory::empty();
        let amount = MAX_STACK_COUNT + 501;

        let outcome = inventory
            .collect_block(BlockId::IronOre, amount)
            .expect("part of the amount fits");
        assert_eq!(outcome.first_slot, 0);
        assert_eq!(outcome.accepted, amount);
        assert_eq!(
            inventory.hotbar(0).map(|item| item.count()),
            Some(MAX_STACK_COUNT)
        );
        assert_eq!(inventory.hotbar(1).map(|item| item.count()), Some(501));
        assert_eq!(
            total_of(&inventory, ItemId::Block(BlockId::IronOre)),
            u32::from(amount)
        );

        // A full inventory accepts nothing and stays untouched.
        let mut inventory = Inventory::empty();
        fill_with(&mut inventory, ItemId::Block(BlockId::Brick));
        let before = inventory.clone();
        assert_eq!(inventory.collect_block(BlockId::Stone, 2), None);
        assert_eq!(inventory, before);

        // A nearly-full inventory accepts exactly the remaining room and
        // reports the shortfall instead of dropping it silently.
        let mut inventory = Inventory::empty();
        for slot in inventory.slots.iter_mut().take(INVENTORY_SLOTS - 1) {
            *slot = Some(block(BlockId::Brick, MAX_STACK_COUNT));
        }
        inventory.slots[INVENTORY_SLOTS - 1] = Some(block(BlockId::Dirt, MAX_STACK_COUNT - 1));
        let outcome = inventory
            .collect_block(BlockId::Dirt, 5)
            .expect("one dirt item still fits");
        assert_eq!(outcome.first_slot, INVENTORY_SLOTS - 1);
        assert_eq!(outcome.accepted, 1);
        assert_eq!(
            total_of(&inventory, ItemId::Block(BlockId::Dirt)),
            u32::from(MAX_STACK_COUNT)
        );
    }

    #[test]
    fn collect_block_rejects_invalid_blocks_and_amount_without_mutating() {
        let mut inventory = Inventory::empty();
        assert_eq!(inventory.collect_block(BlockId::Air, 1), None);
        assert_eq!(inventory.collect_block(BlockId::Bedrock, 1), None);
        assert_eq!(inventory.collect_block(BlockId::Water, 1), None);
        assert_eq!(inventory.collect_block(BlockId::Stone, 0), None);
        assert_eq!(inventory, Inventory::empty());
    }

    #[test]
    fn collect_limited_fills_to_the_authored_limit_then_spills_into_a_fresh_slot() {
        let mut inventory = Inventory::empty();
        let id = ItemId::Item(0);

        let outcome = inventory
            .collect_limited(id, 100, 12)
            .expect("the inventory has room");
        assert_eq!(outcome.first_slot, 0);
        assert_eq!(outcome.accepted, 100);
        assert_eq!(inventory.hotbar(0).map(|item| item.count()), Some(12));
        assert_eq!(inventory.hotbar(1).map(|item| item.count()), Some(12));
        // 8 full slots of 12 plus a final partial stack of 4.
        assert_eq!(inventory.hotbar(8).map(|item| item.count()), Some(4));
        assert_eq!(total_of(&inventory, id), 100);
        assert!(inventory
            .slots()
            .iter()
            .flatten()
            .filter(|item| item.id() == id)
            .all(|item| item.count() <= 12));
    }

    #[test]
    fn collect_limited_merges_an_existing_stack_before_opening_a_new_one() {
        let mut inventory = Inventory::empty();
        let id = ItemId::Item(0);
        inventory.slots[0] = Some(item(id, 10));

        let outcome = inventory
            .collect_limited(id, 5, 12)
            .expect("the remainder fits");
        assert_eq!(outcome.first_slot, 0);
        assert_eq!(outcome.accepted, 5);
        assert_eq!(inventory.hotbar(0).map(|item| item.count()), Some(12));
        assert_eq!(inventory.hotbar(1).map(|item| item.count()), Some(3));
    }

    #[test]
    fn collect_limited_is_a_no_op_with_no_room_or_invalid_input() {
        let id = ItemId::Item(0);

        // Zero amount never mutates.
        let mut inventory = Inventory::empty();
        assert_eq!(inventory.collect_limited(id, 0, 12), None);
        assert_eq!(inventory, Inventory::empty());

        // A non-carryable identity cannot be constructed into a slot.
        assert_eq!(
            inventory.collect_limited(ItemId::Block(BlockId::Air), 1, MAX_STACK_COUNT),
            None
        );
        assert_eq!(inventory, Inventory::empty());

        // A full inventory accepts nothing and stays byte-identical.
        let mut inventory = Inventory::empty();
        fill_with(&mut inventory, ItemId::Block(BlockId::Brick));
        let before = inventory.clone();
        assert_eq!(inventory.collect_limited(id, 1, 12), None);
        assert_eq!(inventory, before);
    }

    #[test]
    fn collect_limited_clamps_degenerate_limits_to_one() {
        let mut inventory = Inventory::empty();
        let id = ItemId::Item(0);
        let outcome = inventory
            .collect_limited(id, 3, 0)
            .expect("three single-item slots fit");
        assert_eq!(outcome.accepted, 3);
        for slot in 0..3 {
            assert_eq!(inventory.hotbar(slot).map(|item| item.count()), Some(1));
        }
    }

    #[test]
    fn collect_limited_with_inserts_a_pack_tool_at_authored_durability() {
        let registry = fixture_registry();
        let id = ItemId::ModTool(0);
        let mut inventory = Inventory::empty();

        let outcome = inventory
            .collect_limited_with(&registry, id, 1, 1)
            .expect("an empty slot accepts the tool");
        assert_eq!(outcome.first_slot, 0);
        assert_eq!(outcome.accepted, 1);
        let slot = inventory.slot(0).expect("the pack tool landed");
        assert_eq!(slot.id(), id);
        assert_eq!(slot.count(), 1);
        assert_eq!(slot.durability(), Some(900));
        assert_eq!(slot.max_durability(), Some(900));

        // Tools are non-stackable: a second unit opens a second slot.
        let outcome = inventory
            .collect_limited_with(&registry, id, 1, 1)
            .expect("a second slot accepts the tool");
        assert_eq!(outcome.first_slot, 1);
        assert_eq!(total_of(&inventory, id), 2);

        // The legacy registry-free path refuses to forge a registry identity.
        assert_eq!(Inventory::empty().collect_limited(id, 1, 1), None);
    }

    #[test]
    fn collect_limited_with_matches_the_legacy_path_for_builtin_drops() {
        let registry = ContentRegistry::builtin();
        let mut legacy = Inventory::empty();
        let mut aware = Inventory::empty();
        let id = ItemId::Block(BlockId::Cobblestone);

        let legacy_outcome = legacy
            .collect_limited(id, 7, MAX_STACK_COUNT)
            .expect("blocks fit");
        let aware_outcome = aware
            .collect_limited_with(registry, id, 7, MAX_STACK_COUNT)
            .expect("blocks fit");
        assert_eq!(legacy_outcome, aware_outcome);
        assert_eq!(legacy.slots(), aware.slots());
    }

    // -- Best-effort batch collection ---------------------------------------

    fn slot_units(inventory: &Inventory) -> u32 {
        inventory
            .slots()
            .iter()
            .flatten()
            .map(|item| u32::from(item.count()))
            .sum()
    }

    #[test]
    fn collect_items_accepts_everything_on_an_empty_inventory_like_insert_all() {
        let items = [
            block(BlockId::Stone, 900),
            stick(30),
            tool(ToolKind::Pickaxe, ToolTier::Iron),
        ];
        let total: u32 = items.iter().map(|item| u32::from(item.count())).sum();

        let mut collected = Inventory::empty();
        let outcome = collected.collect_items(&items);
        assert_eq!(outcome, CollectBatchOutcome::new(total, 0));
        assert!(!outcome.lost_any());

        let mut inserted = Inventory::empty();
        inserted.insert_all(&items).expect("all items fit");
        assert_eq!(
            collected.slots(),
            inserted.slots(),
            "a batch with room left produces the same slots insert_all would"
        );
        assert_eq!(slot_units(&collected), total);
        assert_eq!(collected.cursor(), None, "the cursor is never a target");
    }

    #[test]
    fn collect_items_accepts_the_remaining_room_and_reports_the_rest() {
        // 35 full stick stacks plus one empty slot: only 64 units fit.
        let mut inventory = Inventory::empty();
        for slot in inventory.slots.iter_mut().take(INVENTORY_SLOTS - 1) {
            *slot = Some(stick(MATERIAL_STACK_LIMIT));
        }
        let outcome =
            inventory.collect_items(&[stick(MATERIAL_STACK_LIMIT), stick(MATERIAL_STACK_LIMIT)]);
        assert_eq!(outcome.accepted, u32::from(MATERIAL_STACK_LIMIT));
        assert_eq!(outcome.lost, u32::from(MATERIAL_STACK_LIMIT));
        assert!(outcome.lost_any());
        assert_eq!(
            inventory.slot(INVENTORY_SLOTS - 1).map(|item| item.count()),
            Some(MATERIAL_STACK_LIMIT)
        );

        // 35 full stacks plus one unit of room: exactly one unit is accepted.
        let mut inventory = Inventory::empty();
        for slot in inventory.slots.iter_mut().take(INVENTORY_SLOTS - 1) {
            *slot = Some(stick(MATERIAL_STACK_LIMIT));
        }
        inventory.slots[INVENTORY_SLOTS - 1] = Some(stick(MATERIAL_STACK_LIMIT - 1));
        let outcome = inventory.collect_items(&[stick(MATERIAL_STACK_LIMIT)]);
        assert_eq!(
            outcome,
            CollectBatchOutcome::new(1, u32::from(MATERIAL_STACK_LIMIT) - 1)
        );
        assert_eq!(
            total_of(&inventory, ItemId::Material(Material::Stick)),
            u32::from(MATERIAL_STACK_LIMIT) * 35 + u32::from(MATERIAL_STACK_LIMIT)
        );
    }

    #[test]
    fn collect_items_merges_across_entries_before_using_an_empty_slot() {
        let mut inventory = Inventory::empty();
        inventory.slots[0] = Some(stick(30));

        let outcome = inventory.collect_items(&[stick(20), stick(20)]);
        assert_eq!(outcome, CollectBatchOutcome::new(40, 0));
        assert_eq!(
            inventory.hotbar(0).map(|item| item.count()),
            Some(MATERIAL_STACK_LIMIT),
            "both entries top up the existing stack first"
        );
        assert_eq!(
            inventory.hotbar(1).map(|item| item.count()),
            Some(6),
            "only the true remainder opens a new slot"
        );
        assert_eq!(inventory.hotbar(2), None);
    }

    #[test]
    fn collect_items_never_exceeds_a_stack_limit_in_hotbar_or_main() {
        let mut inventory = Inventory::empty();
        inventory.slots[HOTBAR_SLOTS] = Some(block(BlockId::Stone, MAX_STACK_COUNT - 2));

        let items = [
            block(BlockId::Stone, MAX_STACK_COUNT),
            stick(MATERIAL_STACK_LIMIT),
            stick(5),
        ];
        let outcome = inventory.collect_items(&items);
        assert!(!outcome.lost_any());
        for (index, slot) in inventory.slots().iter().enumerate() {
            if let Some(item) = slot {
                assert!(
                    item.count() <= item.id().stack_limit(),
                    "slot {index} holds {} above the limit {}",
                    item.count(),
                    item.id().stack_limit()
                );
            }
        }
        assert_eq!(
            total_of(&inventory, ItemId::Block(BlockId::Stone)),
            u32::from(MAX_STACK_COUNT) - 2 + u32::from(MAX_STACK_COUNT)
        );
    }

    #[test]
    fn collect_items_on_a_full_inventory_loses_everything_without_mutation() {
        let mut inventory = Inventory::empty();
        fill_with(&mut inventory, ItemId::Block(BlockId::Brick));
        let mut inventory = Inventory::from_parts(*inventory.slots(), Some(stick(3)));
        let before = inventory.clone();

        let outcome = inventory.collect_items(&[
            stick(10),
            tool(ToolKind::Pickaxe, ToolTier::Iron),
            block(BlockId::Dirt, 5),
        ]);
        assert_eq!(outcome, CollectBatchOutcome::new(0, 16));
        assert_eq!(inventory, before, "a full aggregate mutates nothing");
        assert_eq!(inventory.cursor(), Some(stick(3)));
    }

    #[test]
    fn collect_items_gives_a_tool_one_empty_slot_and_loses_the_second() {
        let mut inventory = Inventory::empty();
        for slot in inventory.slots.iter_mut().take(INVENTORY_SLOTS - 1) {
            *slot = Some(block(BlockId::Brick, MAX_STACK_COUNT));
        }

        let pickaxe = tool(ToolKind::Pickaxe, ToolTier::Iron);
        let axe = tool(ToolKind::Axe, ToolTier::Iron);
        let outcome = inventory.collect_items(&[pickaxe, axe]);
        assert_eq!(outcome, CollectBatchOutcome::new(1, 1));
        assert_eq!(inventory.slot(INVENTORY_SLOTS - 1), Some(pickaxe));
        assert_eq!(
            inventory
                .slots()
                .iter()
                .flatten()
                .filter(|item| item.is_tool())
                .count(),
            1,
            "tools never merge and the overflow is lost, not duplicated"
        );
    }

    #[test]
    fn collect_items_conserves_counts_across_hotbar_and_main() {
        let mut inventory = Inventory::empty();
        inventory.slots[0] = Some(block(BlockId::Stone, 10));
        inventory.slots[HOTBAR_SLOTS] = Some(stick(60));
        inventory.slots[HOTBAR_SLOTS + 3] = Some(block(BlockId::Dirt, MAX_STACK_COUNT));
        let before = slot_units(&inventory);

        let items = [stick(20), block(BlockId::Stone, 5), stick(3)];
        let requested: u32 = items.iter().map(|item| u32::from(item.count())).sum();
        let outcome = inventory.collect_items(&items);
        let after = slot_units(&inventory);

        assert_eq!(before + outcome.accepted, after, "no unit is duplicated");
        assert_eq!(
            before + requested - outcome.lost,
            after,
            "only the reported overflow is dropped"
        );
        assert_eq!(
            total_of(&inventory, ItemId::Material(Material::Stick)),
            60 + 20 + 3 - outcome.lost
        );
    }

    #[test]
    fn collect_items_leaves_the_cursor_untouched() {
        let mut inventory = Inventory::empty();
        inventory.slots[0] = Some(block(BlockId::Stone, MAX_STACK_COUNT));
        let mut inventory =
            Inventory::from_parts(*inventory.slots(), Some(block(BlockId::Stone, 7)));

        let outcome = inventory.collect_items(&[block(BlockId::Stone, MAX_STACK_COUNT)]);
        assert_eq!(outcome.accepted, u32::from(MAX_STACK_COUNT));
        assert_eq!(
            inventory.cursor(),
            Some(block(BlockId::Stone, 7)),
            "a matching cursor is never a batch destination"
        );
    }

    #[test]
    fn collect_items_accepts_an_empty_batch_without_mutation() {
        let mut inventory = Inventory::empty();
        inventory.cursor = Some(stick(2));
        let before = inventory.clone();

        assert_eq!(inventory.collect_items(&[]), CollectBatchOutcome::default());
        assert!(!inventory.collect_items(&[]).lost_any());
        assert_eq!(inventory, before);
    }

    #[test]
    fn consume_one_decrements_clears_and_bounds_checks() {
        let mut inventory = Inventory::empty();
        inventory
            .insert(block(BlockId::CoalOre, 2))
            .expect("stack fits");

        let consumed = inventory.consume_one(0).expect("slot has an item");
        assert_eq!(consumed.id(), ItemId::Block(BlockId::CoalOre));
        assert_eq!(consumed.count(), 1);
        assert_eq!(inventory.hotbar(0).map(|item| item.count()), Some(1));

        assert_eq!(
            inventory.consume_one(0).map(|item| item.id()),
            Some(ItemId::Block(BlockId::CoalOre))
        );
        assert_eq!(inventory.hotbar(0), None);
        assert_eq!(inventory.consume_one(0), None);
        assert_eq!(inventory.consume_one(INVENTORY_SLOTS), None);
    }

    #[test]
    fn consume_item_spends_one_unit_and_leaves_the_rest() {
        let mut inventory = Inventory::empty();
        inventory.insert(arrow(4)).expect("stack fits");

        let (index, consumed) = inventory.consume_item(arrow_id()).expect("one unit");
        assert_eq!(index, 0);
        assert_eq!(consumed, arrow(1));
        assert_eq!(inventory.slot(0).map(|item| item.count()), Some(3));
        assert_eq!(total_of(&inventory, arrow_id()), 3);
    }

    #[test]
    fn consume_item_clears_a_single_unit_slot_and_then_reports_none() {
        let mut inventory = Inventory::empty();
        inventory.slots[0] = Some(arrow(1));

        assert_eq!(inventory.consume_item(arrow_id()), Some((0, arrow(1))));
        assert_eq!(inventory.slot(0), None);
        assert_eq!(inventory.consume_item(arrow_id()), None);
        assert_eq!(inventory.slot(0), None);
    }

    #[test]
    fn consume_item_picks_the_earliest_matching_slot_in_flat_order() {
        let mut inventory = Inventory::empty();
        // Main storage first (slot 12), then hotbar (slot 3): the hotbar copy
        // must win because the scan is hotbar-then-main.
        inventory.slots[12] = Some(arrow(2));
        inventory.slots[3] = Some(arrow(5));

        assert_eq!(inventory.consume_item(arrow_id()).map(|(i, _)| i), Some(3));
        assert_eq!(inventory.slot(3).map(|item| item.count()), Some(4));
        assert_eq!(inventory.slot(12).map(|item| item.count()), Some(2));
    }

    #[test]
    fn consume_item_never_touches_the_cursor_or_the_crafting_grids() {
        let mut inventory = Inventory::empty();
        inventory.insert(arrow(2)).expect("stack fits");
        inventory.cursor = Some(arrow(9));
        let before_cursor = inventory.cursor();

        assert!(inventory.consume_item(arrow_id()).is_some());
        assert_eq!(inventory.cursor(), before_cursor);
        assert_eq!(inventory.slot(0).map(|item| item.count()), Some(1));
    }

    #[test]
    fn consume_item_returns_none_without_mutation_for_an_absent_identity() {
        let mut inventory = Inventory::empty();
        inventory.slots[0] = Some(stick(3));
        inventory.slots[9] = Some(block(BlockId::Stone, 1));
        inventory.cursor = Some(arrow(1));
        let before = inventory.clone();

        assert_eq!(inventory.consume_item(arrow_id()), None);
        assert_eq!(inventory, before, "an absent identity mutates nothing");
    }

    fn arrow(count: u16) -> Item {
        item(ItemId::Material(Material::Arrow), count)
    }

    fn arrow_id() -> ItemId {
        ItemId::Material(Material::Arrow)
    }

    // -- Cursor transfers ---------------------------------------------------

    #[test]
    fn pick_up_and_put_down_conserve_items() {
        let mut inventory = Inventory::empty();
        inventory
            .insert(block(BlockId::OakPlanks, 5))
            .expect("stack fits");

        inventory.pick_up(0).expect("empty cursor picks up");
        assert_eq!(inventory.hotbar(0), None);
        assert_eq!(inventory.cursor().map(|item| item.count()), Some(5));

        inventory.put_down(3).expect("empty slot accepts cursor");
        assert_eq!(inventory.cursor(), None);
        assert_eq!(inventory.hotbar(3).map(|item| item.count()), Some(5));
        assert_eq!(total_of(&inventory, ItemId::Block(BlockId::OakPlanks)), 5);
    }

    #[test]
    fn pick_up_merges_same_stack_up_to_the_limit() {
        let mut inventory = Inventory::empty();
        inventory
            .insert(block(BlockId::OakPlanks, 3))
            .expect("cursor source fits");
        inventory.pick_up(0).expect("cursor now holds planks");

        inventory.slots[1] = Some(block(BlockId::OakPlanks, MAX_STACK_COUNT));
        let cursor_before = inventory.cursor().unwrap();
        let slot_before = inventory.slot(1).unwrap();

        inventory
            .pick_up(1)
            .expect("partial merge into a full-ish cursor");

        assert_eq!(
            inventory.cursor().map(|item| item.count()),
            Some(MAX_STACK_COUNT)
        );
        assert_eq!(
            inventory.hotbar(1).map(|item| item.count()),
            Some(slot_before.count() - (MAX_STACK_COUNT - cursor_before.count()))
        );
        assert_eq!(
            total_of(&inventory, ItemId::Block(BlockId::OakPlanks)),
            u32::from(cursor_before.count()) + u32::from(slot_before.count())
        );
    }

    #[test]
    fn pick_up_error_paths_preserve_state() {
        let mut inventory = Inventory::empty();
        assert_eq!(inventory.pick_up(0), Err(InventoryError::EmptySlot));
        assert_eq!(
            inventory.pick_up(INVENTORY_SLOTS),
            Err(InventoryError::OutOfBounds)
        );

        inventory
            .insert(block(BlockId::Stone, 1))
            .expect("stack fits");
        inventory.pick_up(0).expect("cursor holds stone");
        inventory.slots[1] = Some(block(BlockId::OakPlanks, 1));
        let before = inventory.clone();
        assert_eq!(inventory.pick_up(1), Err(InventoryError::CursorOccupied));
        assert_eq!(inventory, before);

        let mut full_cursor = Inventory::empty();
        full_cursor.slots[0] = Some(block(BlockId::OakPlanks, MAX_STACK_COUNT));
        full_cursor.pick_up(0).expect("full stack on cursor");
        full_cursor.slots[1] = Some(block(BlockId::OakPlanks, 1));
        let before = full_cursor.clone();
        assert_eq!(full_cursor.pick_up(1), Err(InventoryError::CursorFull));
        assert_eq!(full_cursor, before);
    }

    #[test]
    fn pick_up_half_rounds_up_and_conserves_items() {
        for count in [1_u16, 2, 3, 4, 5, 9, 64] {
            let mut inventory = Inventory::empty();
            inventory.slots[0] = Some(block(BlockId::OakPlanks, count));

            inventory
                .pick_up_half(0)
                .expect("empty cursor splits the stack");

            let expected_half = count / 2 + count % 2;
            assert_eq!(
                inventory.cursor().map(|item| item.count()),
                Some(expected_half),
                "half rounds up for {count}"
            );
            assert_eq!(
                inventory.hotbar(0).map(|item| item.count()),
                (count > expected_half).then_some(count - expected_half),
                "the remainder stays in the slot for {count}"
            );
            assert_eq!(
                total_of(&inventory, ItemId::Block(BlockId::OakPlanks)),
                u32::from(count),
                "picking half conserves every unit"
            );
        }

        // A tool is a single item: half of one is the whole tool.
        let mut inventory = Inventory::empty();
        inventory.slots[0] = Some(tool(ToolKind::Pickaxe, ToolTier::Wood));
        inventory.pick_up_half(0).expect("a tool moves whole");
        assert_eq!(
            inventory.cursor().map(|item| item.id()),
            Some(ItemId::Tool {
                kind: ToolKind::Pickaxe,
                tier: ToolTier::Wood,
            })
        );
        assert_eq!(inventory.hotbar(0), None);
    }

    #[test]
    fn pick_up_half_error_paths_preserve_state() {
        let mut inventory = Inventory::empty();
        assert_eq!(inventory.pick_up_half(0), Err(InventoryError::EmptySlot));
        assert_eq!(
            inventory.pick_up_half(INVENTORY_SLOTS),
            Err(InventoryError::OutOfBounds)
        );

        inventory.slots[0] = Some(block(BlockId::Stone, 4));
        inventory.pick_up(0).expect("cursor now holds the stack");
        inventory.slots[0] = Some(block(BlockId::Stone, 2));
        let before = inventory.clone();
        assert_eq!(
            inventory.pick_up_half(0),
            Err(InventoryError::CursorOccupied),
            "right-clicking never merges into an occupied cursor"
        );
        assert_eq!(inventory, before);
    }

    #[test]
    fn place_one_moves_exactly_one_unit_and_handles_edge_cases() {
        let mut inventory = Inventory::empty();
        inventory.slots[0] = Some(block(BlockId::OakPlanks, 5));
        inventory.pick_up(0).expect("cursor holds five planks");

        inventory.place_one(1).expect("empty slot accepts one");
        assert_eq!(inventory.hotbar(1).map(|item| item.count()), Some(1));
        assert_eq!(inventory.cursor().map(|item| item.count()), Some(4));

        inventory.slots[2] = Some(block(BlockId::OakPlanks, 2));
        inventory.place_one(2).expect("matching slot accepts one");
        assert_eq!(inventory.hotbar(2).map(|item| item.count()), Some(3));
        assert_eq!(inventory.cursor().map(|item| item.count()), Some(3));
        assert_eq!(
            total_of(&inventory, ItemId::Block(BlockId::OakPlanks)),
            7,
            "place-one conserves the combined count"
        );

        // Full matching slot: rejected without moving anything.
        inventory.slots[3] = Some(block(BlockId::OakPlanks, MAX_STACK_COUNT));
        let before = inventory.clone();
        assert_eq!(inventory.place_one(3), Err(InventoryError::SlotFull));
        assert_eq!(inventory, before);

        // Different identity: rejected without swapping or merging.
        inventory.slots[4] = Some(block(BlockId::Stone, 1));
        let before = inventory.clone();
        assert_eq!(inventory.place_one(4), Err(InventoryError::SlotOccupied));
        assert_eq!(inventory, before);

        assert_eq!(
            inventory.place_one(INVENTORY_SLOTS),
            Err(InventoryError::OutOfBounds)
        );
        assert_eq!(inventory, before);

        // A one-item hand drops the whole item and empties the cursor.
        let mut last = Inventory::empty();
        last.slots[0] = Some(block(BlockId::OakPlanks, 1));
        last.pick_up(0).expect("cursor holds one plank");
        last.place_one(5).expect("place the last item");
        assert_eq!(last.cursor(), None);
        assert_eq!(last.hotbar(5).map(|item| item.count()), Some(1));

        // Empty cursor: explicit error, no mutation.
        let before = last.clone();
        assert_eq!(last.place_one(6), Err(InventoryError::EmptyCursor));
        assert_eq!(last, before);
    }

    #[test]
    fn place_one_moves_a_whole_tool_into_an_empty_slot() {
        let mut inventory = Inventory::empty();
        let pickaxe = tool(ToolKind::Pickaxe, ToolTier::Stone);
        inventory.cursor = Some(pickaxe);

        inventory.place_one(7).expect("a tool moves whole");
        assert_eq!(inventory.cursor(), None);
        assert_eq!(inventory.hotbar(7), Some(pickaxe));

        // An occupied tool slot is not mergeable: rejected, not swapped.
        inventory.pick_up(7).expect("pick the tool back up");
        inventory.slots[8] = Some(tool(ToolKind::Axe, ToolTier::Iron));
        let before = inventory.clone();
        assert_eq!(inventory.place_one(8), Err(InventoryError::SlotOccupied));
        assert_eq!(inventory, before);
    }

    #[test]
    fn put_down_error_paths_preserve_state() {
        let mut inventory = Inventory::empty();
        assert_eq!(inventory.put_down(0), Err(InventoryError::EmptyCursor));

        inventory
            .insert(block(BlockId::Stone, 1))
            .expect("stack fits");
        inventory.pick_up(0).expect("cursor holds stone");
        assert_eq!(
            inventory.put_down(INVENTORY_SLOTS),
            Err(InventoryError::OutOfBounds)
        );
        assert_eq!(
            inventory.cursor().map(|item| item.id()),
            Some(ItemId::Block(BlockId::Stone))
        );

        inventory.slots[0] = Some(block(BlockId::Stone, MAX_STACK_COUNT));
        let before = inventory.clone();
        assert_eq!(inventory.put_down(0), Err(InventoryError::SlotFull));
        assert_eq!(inventory, before);
    }

    #[test]
    fn put_down_swaps_different_items_and_swap_cursor_is_explicit() {
        let mut inventory = Inventory::empty();
        inventory
            .insert(block(BlockId::Stone, 2))
            .expect("stone fits");
        inventory.pick_up(0).expect("cursor holds stone");
        inventory
            .insert(block(BlockId::OakPlanks, 3))
            .expect("planks take slot 0");

        inventory.put_down(0).expect("different item swaps");
        assert_eq!(
            inventory.cursor().map(|item| item.id()),
            Some(ItemId::Block(BlockId::OakPlanks))
        );
        assert_eq!(
            inventory.hotbar(0).map(|item| item.id()),
            Some(ItemId::Block(BlockId::Stone))
        );

        let shovel = tool(ToolKind::Shovel, ToolTier::Iron);
        inventory.insert(shovel).expect("tool fits in slot 1");
        let before = inventory.clone();
        assert_eq!(
            inventory.pick_up(1),
            Err(InventoryError::CursorOccupied),
            "tools never merge onto an occupied cursor"
        );
        assert_eq!(inventory, before);
    }

    #[test]
    fn swap_cursor_swaps_both_sides_and_validates_empties() {
        let mut inventory = Inventory::empty();
        inventory
            .insert(block(BlockId::Stone, 2))
            .expect("stone fits");
        inventory.pick_up(0).expect("cursor holds stone");
        inventory
            .insert(block(BlockId::OakPlanks, 3))
            .expect("planks take slot 0");

        inventory.swap_cursor(0).expect("both sides occupied");
        assert_eq!(
            inventory.cursor().map(|item| item.id()),
            Some(ItemId::Block(BlockId::OakPlanks))
        );
        assert_eq!(
            inventory.hotbar(0).map(|item| item.id()),
            Some(ItemId::Block(BlockId::Stone))
        );

        let mut empty = Inventory::empty();
        assert_eq!(empty.swap_cursor(0), Err(InventoryError::EmptyCursor));
        assert_eq!(empty.pick_up(0), Err(InventoryError::EmptySlot));
    }

    #[test]
    fn take_cursor_and_put_cursor_conserve_items() {
        let mut inventory = Inventory::empty();
        let stack = block(BlockId::Stone, 7);
        inventory.put_cursor(stack).expect("empty cursor accepts");
        assert_eq!(inventory.cursor(), Some(stack));
        assert!(inventory.slots().iter().all(Option::is_none));

        let taken = inventory.take_cursor().expect("cursor occupied");
        assert_eq!(taken, stack, "the whole cursor stack comes back untouched");
        assert_eq!(inventory.cursor(), None);
        assert_eq!(total_of(&inventory, ItemId::Block(BlockId::Stone)), 0);

        inventory.put_cursor(taken).expect("put it back");
        assert_eq!(
            total_of(&inventory, ItemId::Block(BlockId::Stone)),
            7,
            "take/put round-trips conserve identity and count"
        );
    }

    #[test]
    fn put_cursor_rejects_an_occupied_cursor_without_change() {
        let mut inventory = Inventory::empty();
        inventory
            .put_cursor(block(BlockId::Stone, 2))
            .expect("empty cursor accepts");
        let before = inventory.clone();

        assert_eq!(
            inventory.put_cursor(block(BlockId::Stone, 5)),
            Err(InventoryError::CursorOccupied),
            "no merging or replacing at the cursor primitive"
        );
        assert_eq!(inventory, before);

        assert_eq!(
            inventory.put_cursor(block(BlockId::OakPlanks, 1)),
            Err(InventoryError::CursorOccupied),
            "a different identity is rejected just the same"
        );
        assert_eq!(inventory, before);
    }

    #[test]
    fn take_cursor_rejects_an_empty_cursor() {
        let mut inventory = Inventory::empty();
        inventory
            .insert(block(BlockId::Stone, 3))
            .expect("stone fits in slot 0");
        let before = inventory.clone();

        assert_eq!(inventory.take_cursor(), Err(InventoryError::EmptyCursor));
        assert_eq!(
            inventory, before,
            "an empty-cursor rejection leaves slots untouched"
        );
    }

    #[test]
    fn use_tool_wears_down_and_removes_a_broken_tool() {
        let mut inventory = Inventory::empty();
        inventory.slots[0] = Some(
            Item::tool_with_durability(ToolKind::Pickaxe, ToolTier::Wood, 2)
                .expect("durability valid"),
        );

        assert_eq!(
            inventory.use_tool(0),
            Ok(ToolWear::Worn(
                Item::tool_with_durability(ToolKind::Pickaxe, ToolTier::Wood, 1)
                    .expect("durability valid")
            ))
        );
        assert_eq!(
            inventory.hotbar(0).and_then(|item| item.durability()),
            Some(1)
        );

        assert_eq!(inventory.use_tool(0), Ok(ToolWear::Broke));
        assert_eq!(inventory.hotbar(0), None);
        assert_eq!(inventory.use_tool(0), Err(InventoryError::EmptySlot));
        assert_eq!(
            inventory.use_tool(INVENTORY_SLOTS),
            Err(InventoryError::OutOfBounds)
        );

        inventory
            .insert(block(BlockId::Stone, 1))
            .expect("stone fits");
        assert_eq!(inventory.use_tool(0), Err(InventoryError::NotATool));
    }

    // -- Legacy bridge ------------------------------------------------------

    #[test]
    fn legacy_hotbar_bridge_is_lossless_for_block_stacks() {
        let hotbar = Hotbar::from_slots([
            Some(ItemStack::new(BlockId::CoalOre, 3).unwrap()),
            Some(ItemStack::new(BlockId::IronOre, 2).unwrap()),
            None,
            None,
            Some(ItemStack::new(BlockId::Stone, MAX_STACK_COUNT).unwrap()),
            None,
            None,
            None,
            Some(ItemStack::new(BlockId::Brick, 1).unwrap()),
        ]);

        let inventory = Inventory::from_legacy_hotbar(&hotbar);
        assert_eq!(inventory.to_legacy_hotbar(), Some(hotbar.clone()));
        for slot in 0..HOTBAR_SLOTS {
            assert_eq!(inventory.hotbar_stack(slot), hotbar.stack(slot));
        }
        assert!(inventory.slots()[HOTBAR_SLOTS..]
            .iter()
            .all(Option::is_none));
        assert_eq!(inventory.cursor(), None);
    }

    #[test]
    fn legacy_hotbar_view_rejects_non_block_items() {
        let mut inventory = Inventory::empty();
        inventory
            .insert(tool(ToolKind::Axe, ToolTier::Stone))
            .expect("tool fits");
        assert_eq!(inventory.to_legacy_hotbar(), None);
        assert_eq!(inventory.hotbar_stack(0), None);
        assert!(inventory.hotbar(0).is_some());
    }

    #[test]
    fn from_parts_round_trips_cursor_and_slots() {
        let mut slots = [None; INVENTORY_SLOTS];
        slots[2] = Some(block(BlockId::Stone, 4));
        slots[HOTBAR_SLOTS + 5] = Some(tool(ToolKind::Pickaxe, ToolTier::Iron));
        let cursor = Some(stick(7));
        let inventory = Inventory::from_parts(slots, cursor);

        assert_eq!(inventory.slots(), &slots);
        assert_eq!(inventory.cursor(), cursor);
    }

    // -- Crafting grid and recipes -----------------------------------------

    #[test]
    fn crafting_grid_bounds_reject_inactive_cells() {
        let mut player = CraftingGrid::new(GridKind::Player);
        assert_eq!(player.dim(), 2);
        player
            .set(3, Some(block(BlockId::OakLog, 1)))
            .expect("last active cell");
        assert_eq!(
            player.set(4, Some(block(BlockId::OakLog, 1))),
            Err(CraftError::OutOfBounds)
        );
        assert_eq!(player.take(4), Err(CraftError::OutOfBounds));

        let mut table = CraftingGrid::new(GridKind::Table);
        assert_eq!(table.dim(), 4);
        assert_eq!(table.kind().slot_count(), MAX_GRID_SLOTS);
        table
            .set(15, Some(block(BlockId::OakLog, 1)))
            .expect("last active cell");
        assert_eq!(
            table.slot(15).map(|item| item.id()),
            Some(ItemId::Block(BlockId::OakLog)),
            "table cell 15 must be readable"
        );
        assert_eq!(
            table.set(16, Some(block(BlockId::OakLog, 1))),
            Err(CraftError::OutOfBounds)
        );
        assert_eq!(table.take(16), Err(CraftError::OutOfBounds));
        assert_eq!(
            table.take(15).map(|item| item.map(|item| item.id())),
            Ok(Some(ItemId::Block(BlockId::OakLog)))
        );
        assert_eq!(table.take(15), Ok(None));

        let mut slots = [None; MAX_GRID_SLOTS];
        slots[4] = Some(block(BlockId::OakLog, 1));
        assert_eq!(
            CraftingGrid::from_slots(GridKind::Player, slots),
            Err(CraftError::InactiveCell)
        );
        assert!(CraftingGrid::from_slots(GridKind::Table, slots).is_ok());
    }

    #[test]
    fn log_recipe_previews_and_crafts_planks() {
        let mut grid = CraftingGrid::new(GridKind::Player);
        grid.set(0, Some(block(BlockId::OakLog, 1)))
            .expect("cell in range");

        let preview = grid.preview().expect("log matches planks recipe");
        assert_eq!(preview.recipe, RecipeId::Planks);
        assert_eq!(preview.output, ItemId::Block(BlockId::OakPlanks));
        assert_eq!(preview.output_count, 4);

        let mut inventory = Inventory::empty();
        let crafted = grid.take_result(&mut inventory).expect("craft succeeds");
        assert_eq!(crafted.id(), ItemId::Block(BlockId::OakPlanks));
        assert_eq!(crafted.count(), 4);
        assert!(grid.is_empty());
        assert_eq!(total_of(&inventory, ItemId::Block(BlockId::OakPlanks)), 4);
    }

    #[test]
    fn sticks_and_crafting_table_chain_from_planks() {
        let mut sticks_grid = grid_from_pattern(GridKind::Player, &["P", "P"]);
        let preview = sticks_grid.preview().expect("two planks make sticks");
        assert_eq!(preview.recipe, RecipeId::Sticks);
        assert_eq!(preview.output, ItemId::Material(Material::Stick));
        assert_eq!(preview.output_count, 4);

        let mut inventory = Inventory::empty();
        let crafted = sticks_grid
            .take_result(&mut inventory)
            .expect("stick craft succeeds");
        assert_eq!(crafted.id(), ItemId::Material(Material::Stick));
        assert_eq!(crafted.count(), 4);

        let mut table_grid = grid_from_pattern(GridKind::Player, &["PP", "PP"]);
        let preview = table_grid.preview().expect("four planks make a table");
        assert_eq!(preview.recipe, RecipeId::CraftingTable);
        assert_eq!(preview.output, ItemId::Block(BlockId::CraftingTable));
        assert_eq!(preview.output_count, 1);

        let crafted = table_grid
            .take_result(&mut inventory)
            .expect("table craft succeeds");
        assert_eq!(crafted.id(), ItemId::Block(BlockId::CraftingTable));
        assert_eq!(crafted.count(), 1);
    }

    #[test]
    fn chest_recipe_is_a_ring_of_eight_planks_in_the_table_grid() {
        let cells = [0, 1, 2, 4, 6, 8, 9, 10];
        let mut grid = CraftingGrid::new(GridKind::Table);
        for cell in cells {
            grid.set(cell, Some(block(BlockId::OakPlanks, 2)))
                .expect("ring cell in the 4x4 grid");
        }

        let preview = grid.preview().expect("eight planks in a ring make a chest");
        assert_eq!(preview.recipe, RecipeId::Chest);
        assert_eq!(preview.output, ItemId::Block(BlockId::Chest));
        assert_eq!(preview.output_count, 1);

        let mut inventory = Inventory::empty();
        let first = grid.take_result(&mut inventory).expect("first chest");
        assert_eq!(first.id(), ItemId::Block(BlockId::Chest));
        assert_eq!(first.count(), 1);
        for cell in cells {
            assert_eq!(
                grid.slot(cell).map(|item| item.count()),
                Some(1),
                "exactly one plank is consumed from cell {cell}"
            );
        }

        let second = grid.take_result(&mut inventory).expect("second chest");
        assert_eq!(second.id(), ItemId::Block(BlockId::Chest));
        assert!(grid.is_empty(), "eight planks per chest empties the ring");
        assert_eq!(
            total_of(&inventory, ItemId::Block(BlockId::Chest)),
            2,
            "two crafts yield two chests"
        );
        assert_eq!(
            total_of(&inventory, ItemId::Block(BlockId::OakPlanks)),
            0,
            "no planks leak into the inventory"
        );
        assert_eq!(grid.take_result(&mut inventory), Err(CraftError::NoRecipe));
    }

    #[test]
    fn chest_recipe_projections_match_anywhere_in_the_table_grid() {
        // The ring shifted one cell down and right still matches: the matcher
        // compares trimmed bounding boxes, not absolute cells.
        let mut offset = CraftingGrid::new(GridKind::Table);
        for (row, col) in [
            (1, 1),
            (1, 2),
            (1, 3),
            (2, 1),
            (2, 3),
            (3, 1),
            (3, 2),
            (3, 3),
        ] {
            offset
                .set(
                    row * TABLE_GRID_DIM + col,
                    Some(block(BlockId::OakPlanks, 1)),
                )
                .expect("cell in range");
        }
        assert_eq!(
            offset.preview().map(|preview| preview.recipe),
            Some(RecipeId::Chest)
        );
    }

    #[test]
    fn chest_recipe_cannot_be_crafted_in_the_2x2_player_grid() {
        // Four planks in the player grid are the crafting-table recipe; the
        // chest's 3x3 bounding box can never fit there.
        let mut player_grid = grid_from_pattern(GridKind::Player, &["PP", "PP"]);
        assert_eq!(
            player_grid.preview().map(|preview| preview.recipe),
            Some(RecipeId::CraftingTable)
        );

        let mut inventory = Inventory::empty();
        let crafted = player_grid
            .take_result(&mut inventory)
            .expect("the 2x2 grid still crafts a table");
        assert_eq!(crafted.id(), ItemId::Block(BlockId::CraftingTable));
        assert_eq!(
            total_of(&inventory, ItemId::Block(BlockId::Chest)),
            0,
            "a chest can never come out of a 2x2 craft"
        );

        // Even a 2x2 arrangement of planks with a hole is not the chest: the
        // only four-plank player recipe is the table.
        let mut holed = CraftingGrid::new(GridKind::Player);
        for index in [0, 1, 2] {
            holed
                .set(index, Some(block(BlockId::OakPlanks, 1)))
                .expect("cell in range");
        }
        assert_eq!(holed.preview(), None);
    }

    #[test]
    fn every_tool_recipe_is_shaped_and_produces_a_full_durability_instance() {
        let cases: [(RecipeId, ToolKind, ToolTier, &[&str]); 16] = [
            (
                RecipeId::WoodPickaxe,
                ToolKind::Pickaxe,
                ToolTier::Wood,
                &["PPP", " S ", " S "],
            ),
            (
                RecipeId::WoodAxe,
                ToolKind::Axe,
                ToolTier::Wood,
                &["PP ", "PS ", " S "],
            ),
            (
                RecipeId::WoodShovel,
                ToolKind::Shovel,
                ToolTier::Wood,
                &["P", "S", "S"],
            ),
            (
                RecipeId::StonePickaxe,
                ToolKind::Pickaxe,
                ToolTier::Stone,
                &["CCC", " S ", " S "],
            ),
            (
                RecipeId::StoneAxe,
                ToolKind::Axe,
                ToolTier::Stone,
                &["CC ", "CS ", " S "],
            ),
            (
                RecipeId::StoneShovel,
                ToolKind::Shovel,
                ToolTier::Stone,
                &["C", "S", "S"],
            ),
            (
                RecipeId::IronPickaxe,
                ToolKind::Pickaxe,
                ToolTier::Iron,
                &["III", " S ", " S "],
            ),
            (
                RecipeId::IronAxe,
                ToolKind::Axe,
                ToolTier::Iron,
                &["II ", "IS ", " S "],
            ),
            (
                RecipeId::IronShovel,
                ToolKind::Shovel,
                ToolTier::Iron,
                &["I", "S", "S"],
            ),
            (
                RecipeId::WoodHoe,
                ToolKind::Hoe,
                ToolTier::Wood,
                &["PP", " S", " S"],
            ),
            (
                RecipeId::StoneHoe,
                ToolKind::Hoe,
                ToolTier::Stone,
                &["CC", " S", " S"],
            ),
            (
                RecipeId::IronHoe,
                ToolKind::Hoe,
                ToolTier::Iron,
                &["II", " S", " S"],
            ),
            (
                RecipeId::WoodSword,
                ToolKind::Sword,
                ToolTier::Wood,
                &["P", "P", "S"],
            ),
            (
                RecipeId::StoneSword,
                ToolKind::Sword,
                ToolTier::Stone,
                &["C", "C", "S"],
            ),
            (
                RecipeId::IronSword,
                ToolKind::Sword,
                ToolTier::Iron,
                &["I", "I", "S"],
            ),
            (
                RecipeId::Shortbow,
                ToolKind::Bow,
                ToolTier::Wood,
                &[" SE", "S E", " S "],
            ),
        ];

        for (recipe_id, kind, tier, pattern) in cases {
            let mut grid = grid_from_pattern(GridKind::Table, pattern);
            let preview = grid.preview().expect("tool pattern matches");
            assert_eq!(preview.recipe, recipe_id);
            assert_eq!(
                preview.output,
                ItemId::Tool { kind, tier },
                "{recipe_id:?} identity"
            );

            let mut inventory = Inventory::empty();
            let crafted = grid
                .take_result(&mut inventory)
                .expect("tool craft succeeds");
            assert_eq!(crafted.id(), ItemId::Tool { kind, tier });
            assert_eq!(crafted.count(), 1);
            assert_eq!(crafted.durability(), Some(tier.max_durability()));
            assert!(grid.is_empty());
        }
    }

    #[test]
    fn recipe_matching_ignores_offset_but_rejects_extra_items() {
        let mut offset = CraftingGrid::new(GridKind::Table);
        offset
            .set(4, Some(block(BlockId::OakLog, 1)))
            .expect("center cell");
        assert_eq!(
            offset.preview().map(|preview| preview.recipe),
            Some(RecipeId::Planks)
        );

        let mut extra = grid_from_pattern(GridKind::Player, &["P", "P"]);
        extra
            .set(1, Some(block(BlockId::OakLog, 1)))
            .expect("extra cell");
        assert_eq!(extra.preview(), None);

        let mut empty = CraftingGrid::new(GridKind::Table);
        assert_eq!(empty.preview(), None);
        let mut inventory = Inventory::empty();
        assert_eq!(empty.take_result(&mut inventory), Err(CraftError::NoRecipe));
        assert_eq!(inventory, Inventory::empty());
    }

    #[test]
    fn player_grid_cannot_fit_table_only_patterns() {
        let mut player = CraftingGrid::new(GridKind::Player);
        player
            .set(0, Some(block(BlockId::OakPlanks, 1)))
            .expect("cell 0");
        player.set(1, Some(stick(1))).expect("cell 1");
        // A horizontal plank+stick pair is no recipe at all; the three-tall
        // tool patterns cannot fit a 2x2 grid.
        assert_eq!(player.preview(), None);

        let mut table = CraftingGrid::new(GridKind::Table);
        table
            .set(0, Some(block(BlockId::OakPlanks, 1)))
            .expect("cell 0");
        table
            .set(TABLE_GRID_DIM, Some(stick(1)))
            .expect("cell below 0");
        table
            .set(2 * TABLE_GRID_DIM, Some(stick(1)))
            .expect("cell two below 0");
        assert_eq!(
            table.preview().map(|preview| preview.recipe),
            Some(RecipeId::WoodShovel)
        );
    }

    #[test]
    fn bread_recipe_requires_one_three_wheat_row_in_the_table() {
        let mut grid = grid_from_pattern(GridKind::Table, &["WWW"]);
        let preview = grid.preview().expect("three wheat in a row match bread");
        assert_eq!(preview.recipe, RecipeId::Bread);
        assert_eq!(preview.output, ItemId::Material(Material::Bread));
        assert_eq!(preview.output_count, 1);

        let mut inventory = Inventory::empty();
        let crafted = grid.take_result(&mut inventory).expect("bread craft");
        assert_eq!(crafted.id(), ItemId::Material(Material::Bread));
        assert_eq!(crafted.count(), 1);
        assert_eq!(total_of(&inventory, ItemId::Material(Material::Wheat)), 0);
        assert!(grid.is_empty());
    }

    #[test]
    fn bread_recipe_rejects_short_gapped_and_cramped_grids() {
        // Two wheat is not a loaf.
        let two = grid_from_pattern(GridKind::Table, &["WW"]);
        assert_eq!(two.preview(), None);

        // Three wheat with a hole keeps a three-wide bounding box but fails
        // the inner-space check.
        let mut gapped = CraftingGrid::new(GridKind::Table);
        for index in [0, 2] {
            gapped.set(index, Some(wheat(1))).expect("cell in range");
        }
        assert_eq!(gapped.preview(), None);

        // The 2x2 player grid cannot hold a three-wide row: packing three
        // wheat into an L leaves no recipe match.
        let mut player = CraftingGrid::new(GridKind::Player);
        for index in [0, 1, 2] {
            player.set(index, Some(wheat(1))).expect("cell in range");
        }
        assert_eq!(player.preview(), None);
    }

    #[test]
    fn player_grid_cannot_fit_the_two_by_three_hoe_pattern() {
        // The hoe bounding box is 2 wide and 3 tall, so the 2x2 player grid
        // can at most hold its top row plus one stick below. That prefix must
        // stay recipe-less; the full pattern is table-only.
        let mut player = CraftingGrid::new(GridKind::Player);
        player
            .set(0, Some(block(BlockId::OakPlanks, 1)))
            .expect("cell 0");
        player
            .set(1, Some(block(BlockId::OakPlanks, 1)))
            .expect("cell 1");
        player.set(3, Some(stick(1))).expect("cell 3");
        assert_eq!(player.preview(), None);
    }

    #[test]
    fn sword_recipes_are_table_only() {
        let cases: [(RecipeId, &[&str]); 3] = [
            (RecipeId::WoodSword, &["P", "P", "S"]),
            (RecipeId::StoneSword, &["C", "C", "S"]),
            (RecipeId::IronSword, &["I", "I", "S"]),
        ];
        for (recipe_id, pattern) in cases {
            // Swords are a 1-wide, 3-tall column while the player grid is two
            // rows tall, so the full shape can never be placed there.
            assert_eq!(pattern.len(), 3);
            assert!(GridKind::Player.dim() < pattern.len());

            let ids = [
                pattern_item(pattern[0].as_bytes()[0]).expect("head"),
                pattern_item(pattern[1].as_bytes()[0]).expect("head"),
                pattern_item(pattern[2].as_bytes()[0]).expect("handle"),
            ];

            // Placing the only rows a 2x2 grid can hold (both heads) must
            // never produce the sword. The wood sword's two planks are
            // legitimately the sticks recipe, so this asserts "not this
            // sword" rather than "no recipe".
            let mut partial = CraftingGrid::new(GridKind::Player);
            partial.set(0, Some(item(ids[0], 1))).expect("cell 0");
            partial
                .set(GridKind::Player.dim(), Some(item(ids[1], 1)))
                .expect("cell 2");
            assert_ne!(
                partial.preview().map(|preview| preview.recipe),
                Some(recipe_id),
                "{recipe_id:?} cannot be completed in the 2x2 player grid"
            );

            // Even squeezing all three ingredients into the 2x2 L leaves no
            // sword recipe.
            let mut packed = CraftingGrid::new(GridKind::Player);
            for (index, id) in ids.into_iter().enumerate() {
                packed.set(index, Some(item(id, 1))).expect("player cell");
            }
            assert_eq!(
                packed.preview(),
                None,
                "{recipe_id:?} cannot fit the 2x2 player grid"
            );

            let table = grid_from_pattern(GridKind::Table, pattern);
            assert_eq!(
                table.preview().map(|preview| preview.recipe),
                Some(recipe_id),
                "{recipe_id:?} matches on the table"
            );
        }
    }

    #[test]
    fn sword_recipe_matches_at_an_offset_on_the_table() {
        let mut grid = CraftingGrid::new(GridKind::Table);
        let dim = GridKind::Table.dim();
        // Wood sword shifted one row down and one column right.
        grid.set(dim + 1, Some(block(BlockId::OakPlanks, 1)))
            .expect("first head cell");
        grid.set(2 * dim + 1, Some(block(BlockId::OakPlanks, 1)))
            .expect("second head cell");
        grid.set(3 * dim + 1, Some(stick(1))).expect("handle cell");
        assert_eq!(
            grid.preview().map(|preview| preview.recipe),
            Some(RecipeId::WoodSword)
        );

        let mut inventory = Inventory::empty();
        let crafted = grid.take_result(&mut inventory).expect("offset craft");
        assert_eq!(
            crafted.id(),
            ItemId::Tool {
                kind: ToolKind::Sword,
                tier: ToolTier::Wood,
            }
        );
        assert!(grid.is_empty());
    }

    #[test]
    fn sword_recipe_rejects_a_wrong_head_material() {
        // Both heads must be the sword tier's material; mixing tiers in the
        // head column matches nothing.
        let mixed_heads = grid_from_pattern(GridKind::Table, &["P", "C", "S"]);
        assert_eq!(mixed_heads.preview(), None);

        // A plank in the handle cell is not a wood sword either.
        let wrong_handle = grid_from_pattern(GridKind::Table, &["P", "P", "P"]);
        assert_eq!(wrong_handle.preview(), None);

        // And an iron-column handle with stone heads is still not a sword.
        let iron_handle = grid_from_pattern(GridKind::Table, &["C", "C", "I"]);
        assert_eq!(iron_handle.preview(), None);
    }

    #[test]
    fn wood_sword_craft_chain_starts_from_one_oak_log() {
        // The slice demo opens with "craft a wood sword from planks and
        // sticks". Every step moves items between the inventory and a grid,
        // so the totals account for every unit: nothing is spawned.
        let mut inventory = Inventory::empty();
        inventory
            .insert(block(BlockId::OakLog, 1))
            .expect("inventory holds the starting log");

        // Step 1: the single oak log becomes four planks in the 2x2 grid.
        let mut player = CraftingGrid::new(GridKind::Player);
        let log = withdraw(&mut inventory, ItemId::Block(BlockId::OakLog), 1);
        player.set(0, Some(log)).expect("player cell");
        let planks = player.take_result(&mut inventory).expect("planks craft");
        assert_eq!(planks.id(), ItemId::Block(BlockId::OakPlanks));
        assert_eq!(planks.count(), 4);
        assert_eq!(total_of(&inventory, ItemId::Block(BlockId::OakLog)), 0);
        assert_eq!(total_of(&inventory, ItemId::Block(BlockId::OakPlanks)), 4);
        assert_eq!(total_of(&inventory, ItemId::Material(Material::Stick)), 0);
        assert!(player.is_empty());

        // Step 2: two of those four planks become four sticks.
        let mut player = CraftingGrid::new(GridKind::Player);
        player
            .set(
                0,
                Some(withdraw(
                    &mut inventory,
                    ItemId::Block(BlockId::OakPlanks),
                    1,
                )),
            )
            .expect("player cell");
        player
            .set(
                GridKind::Player.dim(),
                Some(withdraw(
                    &mut inventory,
                    ItemId::Block(BlockId::OakPlanks),
                    1,
                )),
            )
            .expect("player cell");
        let sticks = player.take_result(&mut inventory).expect("sticks craft");
        assert_eq!(sticks.id(), ItemId::Material(Material::Stick));
        assert_eq!(sticks.count(), 4);
        assert_eq!(
            total_of(&inventory, ItemId::Block(BlockId::OakPlanks)),
            2,
            "two planks remain"
        );
        assert_eq!(total_of(&inventory, ItemId::Material(Material::Stick)), 4);
        assert!(player.is_empty());

        // Step 3: the two remaining planks plus one stick become the sword.
        let mut table = CraftingGrid::new(GridKind::Table);
        let dim = table.dim();
        for index in [0, dim] {
            table
                .set(
                    index,
                    Some(withdraw(
                        &mut inventory,
                        ItemId::Block(BlockId::OakPlanks),
                        1,
                    )),
                )
                .expect("head cell");
        }
        table
            .set(
                2 * dim,
                Some(withdraw(
                    &mut inventory,
                    ItemId::Material(Material::Stick),
                    1,
                )),
            )
            .expect("handle cell");

        let preview = table.preview().expect("wood sword pattern matches");
        assert_eq!(preview.recipe, RecipeId::WoodSword);

        let sword = table.take_result(&mut inventory).expect("sword craft");
        assert_eq!(
            sword.id(),
            ItemId::Tool {
                kind: ToolKind::Sword,
                tier: ToolTier::Wood,
            }
        );
        assert_eq!(sword.count(), 1);
        assert_eq!(sword.durability(), Some(59));
        assert!(table.is_empty());

        // Conservation: the log and both head planks are gone, the handle
        // consumed exactly one stick, and the sword is the only new stack.
        assert_eq!(total_of(&inventory, ItemId::Block(BlockId::OakLog)), 0);
        assert_eq!(total_of(&inventory, ItemId::Block(BlockId::OakPlanks)), 0);
        assert_eq!(
            total_of(&inventory, ItemId::Material(Material::Stick)),
            3,
            "only the handle stick is consumed"
        );
        assert_eq!(total_of(&inventory, sword.id()), 1);
    }

    #[test]
    fn take_result_revalidates_a_stale_preview() {
        let mut grid = grid_from_pattern(GridKind::Player, &["P", "P"]);
        let stale = grid.preview().expect("initial preview");
        assert_eq!(stale.recipe, RecipeId::Sticks);

        grid.set(2, None).expect("remove one plank");
        assert_eq!(grid.preview(), None);

        let mut inventory = Inventory::empty();
        assert_eq!(grid.take_result(&mut inventory), Err(CraftError::NoRecipe));
        assert_eq!(
            grid.slot(0).map(|item| item.count()),
            Some(1),
            "inputs are untouched on a failed craft"
        );
        assert_eq!(inventory, Inventory::empty());
    }

    #[test]
    fn take_result_rejects_a_blocked_destination_without_consuming() {
        let mut grid = grid_from_pattern(GridKind::Player, &["L"]);
        let mut inventory = Inventory::empty();
        fill_with(&mut inventory, ItemId::Block(BlockId::Brick));

        assert_eq!(
            grid.take_result(&mut inventory),
            Err(CraftError::DestinationFull)
        );
        assert_eq!(
            grid.slot(0).map(|item| item.id()),
            Some(ItemId::Block(BlockId::OakLog))
        );
        assert_eq!(total_of(&inventory, ItemId::Block(BlockId::OakLog)), 0);
    }

    #[test]
    fn take_result_consumes_each_input_cell_exactly_once() {
        let mut grid = CraftingGrid::new(GridKind::Player);
        grid.set(0, Some(block(BlockId::OakPlanks, 5)))
            .expect("cell 0");
        grid.set(2, Some(block(BlockId::OakPlanks, 5)))
            .expect("cell 2");

        let mut inventory = Inventory::empty();
        grid.take_result(&mut inventory).expect("first craft");
        assert_eq!(
            (
                grid.slot(0).map(|item| item.count()),
                grid.slot(2).map(|item| item.count())
            ),
            (Some(4), Some(4))
        );
        assert_eq!(total_of(&inventory, ItemId::Material(Material::Stick)), 4);

        grid.take_result(&mut inventory).expect("second craft");
        assert_eq!(
            (
                grid.slot(0).map(|item| item.count()),
                grid.slot(2).map(|item| item.count())
            ),
            (Some(3), Some(3))
        );
        assert_eq!(total_of(&inventory, ItemId::Material(Material::Stick)), 8);
        assert_eq!(
            total_of(&inventory, ItemId::Block(BlockId::OakPlanks)),
            0,
            "planks stay in the grid until the recipe consumes them"
        );
    }

    #[test]
    fn ranged_recipes_are_reachable_without_a_furnace() {
        // -- Shortbow: 3 sticks + 2 leather, table-only by shape ------------
        let bow_pattern = [" SE", "S E", " S "];
        assert_eq!(bow_pattern.len(), 3);
        assert!(GridKind::Player.dim() < bow_pattern.len());

        let bow_grid = grid_from_pattern(GridKind::Table, &bow_pattern);
        let preview = bow_grid.preview().expect("shortbow pattern matches");
        assert_eq!(preview.recipe, RecipeId::Shortbow);
        assert_eq!(
            preview.output,
            ItemId::Tool {
                kind: ToolKind::Bow,
                tier: ToolTier::Wood,
            }
        );
        assert_eq!(preview.output_count, 1);

        // The 2x2 player grid is only two rows tall, so the bow can never be
        // completed there even when every cell is filled with a bow material.
        let mut player_grid = CraftingGrid::new(GridKind::Player);
        player_grid.set(0, Some(stick(1))).expect("cell 0");
        player_grid
            .set(1, Some(item(ItemId::Material(Material::Leather), 1)))
            .expect("cell 1");
        player_grid.set(2, Some(stick(1))).expect("cell 2");
        player_grid
            .set(3, Some(item(ItemId::Material(Material::Leather), 1)))
            .expect("cell 3");
        assert_ne!(
            player_grid.preview().map(|preview| preview.recipe),
            Some(RecipeId::Shortbow),
            "the shortbow must stay table-only"
        );

        // -- Arrows: 1 stick + 1 coal ore -> 4 arrows -----------------------
        let arrow_grid = grid_from_pattern(GridKind::Player, &["S", "K"]);
        let preview = arrow_grid
            .preview()
            .expect("arrow pattern matches on the 2x2 player grid");
        assert_eq!(preview.recipe, RecipeId::Arrows);
        assert_eq!(preview.output, ItemId::Material(Material::Arrow));
        assert_eq!(preview.output_count, 4);

        let table_arrow_grid = grid_from_pattern(GridKind::Table, &["S", "K"]);
        assert_eq!(
            table_arrow_grid.preview().map(|preview| preview.recipe),
            Some(RecipeId::Arrows),
            "the same arrow shape also matches on the table"
        );

        // -- Full chain: one oak log + 2 leather + 1 coal ore ---------------
        //
        // Every step moves items between the inventory and a grid, so the
        // totals account for every unit: nothing is spawned.
        let mut inventory = Inventory::empty();
        inventory
            .insert(block(BlockId::OakLog, 1))
            .expect("inventory holds the starting log");
        inventory
            .insert(item(ItemId::Material(Material::Leather), 2))
            .expect("inventory holds the leather");
        inventory
            .insert(item(ItemId::Block(BlockId::CoalOre), 1))
            .expect("inventory holds the coal ore");

        // Step 1: the single oak log becomes four planks in the 2x2 grid.
        let mut player = CraftingGrid::new(GridKind::Player);
        player
            .set(
                0,
                Some(withdraw(&mut inventory, ItemId::Block(BlockId::OakLog), 1)),
            )
            .expect("player cell");
        let planks = player.take_result(&mut inventory).expect("planks craft");
        assert_eq!(planks.id(), ItemId::Block(BlockId::OakPlanks));
        assert_eq!(planks.count(), 4);
        assert!(player.is_empty());

        // Step 2: two rounds of two planks become four sticks each.
        for round in 0..2 {
            let mut sticks_grid = CraftingGrid::new(GridKind::Player);
            for index in [0, GridKind::Player.dim()] {
                sticks_grid
                    .set(
                        index,
                        Some(withdraw(
                            &mut inventory,
                            ItemId::Block(BlockId::OakPlanks),
                            1,
                        )),
                    )
                    .expect("plank cell");
            }
            let crafted = sticks_grid
                .take_result(&mut inventory)
                .expect("planks craft into sticks");
            assert_eq!(crafted.id(), ItemId::Material(Material::Stick));
            assert_eq!(crafted.count(), 4, "round {round}");
            assert!(sticks_grid.is_empty());
        }
        assert_eq!(
            total_of(&inventory, ItemId::Block(BlockId::OakPlanks)),
            0,
            "one log is exactly four planks and eight sticks"
        );
        assert_eq!(total_of(&inventory, ItemId::Material(Material::Stick)), 8);

        // Step 3: three sticks plus two leather become one shortbow.
        let mut table = CraftingGrid::new(GridKind::Table);
        let dim = table.dim();
        let bow_cells = [
            (1, ItemId::Material(Material::Stick)),
            (2, ItemId::Material(Material::Leather)),
            (dim, ItemId::Material(Material::Stick)),
            (dim + 2, ItemId::Material(Material::Leather)),
            (2 * dim + 1, ItemId::Material(Material::Stick)),
        ];
        for (index, id) in bow_cells {
            let ingredient = withdraw(&mut inventory, id, 1);
            table.set(index, Some(ingredient)).expect("bow cell");
        }
        assert_eq!(
            table.preview().map(|preview| preview.recipe),
            Some(RecipeId::Shortbow)
        );
        let bow_item = table.take_result(&mut inventory).expect("bow craft");
        assert_eq!(
            bow_item.id(),
            ItemId::Tool {
                kind: ToolKind::Bow,
                tier: ToolTier::Wood,
            }
        );
        assert_eq!(bow_item.count(), 1);
        assert_eq!(
            bow_item.durability(),
            Some(59),
            "a wood bow starts at the full wood-tier durability"
        );
        assert!(table.is_empty(), "the bow consumes all five cells");

        // Step 4: one stick plus the coal ore become four arrows, entirely in
        // the 2x2 player grid.
        let mut arrow_cells = CraftingGrid::new(GridKind::Player);
        arrow_cells
            .set(
                0,
                Some(withdraw(
                    &mut inventory,
                    ItemId::Material(Material::Stick),
                    1,
                )),
            )
            .expect("stick cell");
        arrow_cells
            .set(
                GridKind::Player.dim(),
                Some(withdraw(&mut inventory, ItemId::Block(BlockId::CoalOre), 1)),
            )
            .expect("coal cell");
        assert_eq!(
            arrow_cells.preview().map(|preview| preview.recipe),
            Some(RecipeId::Arrows)
        );
        let arrows = arrow_cells
            .take_result(&mut inventory)
            .expect("arrow craft");
        assert_eq!(arrows.id(), ItemId::Material(Material::Arrow));
        assert_eq!(arrows.count(), 4);
        assert!(arrow_cells.is_empty(), "the arrows consume both cells");

        // Exact unit conservation: eight sticks paid 3 for the bow and 1 for
        // the arrows, so four remain; both leather and the coal ore are spent.
        assert_eq!(total_of(&inventory, ItemId::Material(Material::Stick)), 4);
        assert_eq!(total_of(&inventory, ItemId::Material(Material::Arrow)), 4);
        assert_eq!(total_of(&inventory, ItemId::Material(Material::Leather)), 0);
        assert_eq!(total_of(&inventory, ItemId::Block(BlockId::CoalOre)), 0);
        assert_eq!(total_of(&inventory, bow_item.id()), 1);
        let tools: Vec<ItemId> = inventory
            .slots()
            .iter()
            .flatten()
            .filter(|item| item.id().is_tool())
            .map(|item| item.id())
            .collect();
        assert_eq!(
            tools,
            vec![ItemId::Tool {
                kind: ToolKind::Bow,
                tier: ToolTier::Wood,
            }],
            "the chain produces exactly one tool: the shortbow"
        );
    }

    #[test]
    fn recipe_table_patterns_are_unambiguous() {
        assert_eq!(
            RECIPES.len(),
            22,
            "13 pre-farming + 4 farming + 3 swords + 2 ranged"
        );
        for recipe in RECIPES {
            let mut grid = CraftingGrid::new(GridKind::Table);
            for (row, line) in recipe.pattern.iter().enumerate() {
                for (col, byte) in line.bytes().enumerate() {
                    if byte == b' ' {
                        continue;
                    }
                    let id = pattern_item(byte).expect("static pattern characters are known");
                    grid.set(row * GridKind::Table.dim() + col, Some(item(id, 1)))
                        .expect("static patterns fit the table grid");
                }
            }
            assert_eq!(
                grid.preview().map(|preview| preview.recipe),
                Some(recipe.id),
                "{:?} must be the only recipe matching its own pattern",
                recipe.id
            );
        }
    }

    // -- Registry-aware shaped matching (S05-T02) ---------------------------

    #[test]
    fn pack_recipes_preview_and_craft_in_both_grids() {
        let registry = recipe_fixture_registry();

        // 2x2 player grid: four stone make four authored gems.
        let mut player_grid = CraftingGrid::new(GridKind::Player);
        for index in 0..4 {
            player_grid
                .set(index, Some(block(BlockId::Stone, 1)))
                .expect("2x2 cell");
        }
        let gem_recipe = registry
            .recipe_by_name("mymod:gem_from_stone")
            .expect("gem recipe exists");
        let preview = player_grid
            .preview_with(&registry)
            .expect("the pack gem recipe matches the player grid");
        assert_eq!(preview.recipe, RecipeId::Modded(gem_recipe.index));
        assert_eq!(preview.output, ItemId::Item(0));
        assert_eq!(preview.output_count, 4);

        let mut inventory = Inventory::empty();
        let crafted = player_grid
            .take_result_with(&registry, &mut inventory)
            .expect("pack gem recipe crafts");
        assert_eq!(crafted.id(), ItemId::Item(0));
        assert_eq!(crafted.count(), 4);
        assert_eq!(
            inventory.slot(0).map(|item| item.id()),
            Some(ItemId::Item(0))
        );
        assert!(player_grid.is_empty());
        assert!(
            Item::new_with(&registry, ItemId::Item(0), 13).is_err(),
            "the authored stack_limit 12 caps the crafted item"
        );

        // 4x4 table grid: three gems over two sticks make the pack tool.
        let mut table_grid = CraftingGrid::new(GridKind::Table);
        for (row, line) in ["RRR", " S ", " S "].iter().enumerate() {
            for (col, byte) in line.bytes().enumerate() {
                let cell = match byte {
                    b'R' => {
                        Item::new_with(&registry, ItemId::Item(0), 1).expect("authored gem item")
                    }
                    b'S' => stick(1),
                    _ => continue,
                };
                table_grid
                    .set(row * GridKind::Table.dim() + col, Some(cell))
                    .expect("4x4 cell");
            }
        }
        let pickaxe_recipe = registry
            .recipe_by_name("mymod:ruby_pickaxe")
            .expect("pickaxe recipe exists");
        let preview = table_grid
            .preview_with(&registry)
            .expect("the pack pickaxe recipe matches the table grid");
        assert_eq!(preview.recipe, RecipeId::Modded(pickaxe_recipe.index));
        assert_eq!(preview.output, ItemId::ModTool(0));
        assert_eq!(preview.output_count, 1);

        let crafted = table_grid
            .take_result_with(&registry, &mut Inventory::empty())
            .expect("pack pickaxe crafts");
        assert_eq!(crafted.id(), ItemId::ModTool(0));
        assert_eq!(crafted.count(), 1);
        assert_eq!(crafted.durability(), Some(900));
        assert_eq!(crafted.max_durability(), Some(900));
        assert!(table_grid.is_empty());
    }

    #[test]
    fn disabled_builtin_recipe_never_previews_or_crafts() {
        use crate::content::spec::RecipePatch;

        let mut spec = crate::content::spec::PackContentSpec::default();
        spec.recipe_patches.push(RecipePatch {
            id: "shortbow".to_string(),
            enabled: false,
        });
        let (registry, report) = crate::content::build_registry(
            ContentRegistry::builtin(),
            &[(
                crate::content::PackIdentity {
                    folder: "fixture".to_string(),
                    version: None,
                },
                spec,
            )],
        );
        assert_eq!(report.errors(), 0);

        let grid = grid_from_pattern(GridKind::Table, &[" SE", "S E", " S "]);
        assert!(
            grid.preview().is_some(),
            "the frozen built-in table still knows the shortbow"
        );
        assert_eq!(
            grid.preview_with(&registry),
            None,
            "a disabled recipe produces no preview"
        );

        let mut disabled_grid = grid.clone();
        let mut inventory = Inventory::empty();
        assert_eq!(
            disabled_grid.take_result_with(&registry, &mut inventory),
            Err(CraftError::NoRecipe)
        );
        assert_eq!(disabled_grid, grid, "the grid is untouched");
        assert_eq!(inventory, Inventory::empty(), "nothing was inserted");
    }

    #[test]
    fn partially_filled_grid_never_matches_a_pack_recipe() {
        let registry = recipe_fixture_registry();
        let mut grid = CraftingGrid::new(GridKind::Player);
        for index in 0..3 {
            grid.set(index, Some(block(BlockId::Stone, 1)))
                .expect("2x2 cell");
        }
        assert_eq!(grid.preview_with(&registry), None);
        let before = grid.clone();
        let mut inventory = Inventory::empty();
        assert_eq!(
            grid.take_result_with(&registry, &mut inventory),
            Err(CraftError::NoRecipe)
        );
        assert_eq!(grid, before);
        assert_eq!(inventory, Inventory::empty());
    }

    #[test]
    fn builtin_and_registry_aware_paths_answer_identically_for_every_builtin_recipe() {
        let registry = ContentRegistry::builtin();
        for recipe in RECIPES {
            let grid = grid_from_pattern(GridKind::Table, recipe.pattern);
            let mut legacy_grid = grid.clone();
            let mut aware_grid = grid.clone();
            assert_eq!(
                legacy_grid.preview(),
                aware_grid.preview_with(registry),
                "preview mismatch for {:?}",
                recipe.id
            );

            let mut legacy_inventory = Inventory::empty();
            let mut aware_inventory = Inventory::empty();
            assert_eq!(
                legacy_grid.take_result(&mut legacy_inventory),
                aware_grid.take_result_with(registry, &mut aware_inventory),
                "craft mismatch for {:?}",
                recipe.id
            );
            assert_eq!(legacy_grid, aware_grid);
            assert_eq!(legacy_inventory, aware_inventory);
        }
    }

    #[test]
    fn default_crafting_grid_is_an_empty_player_grid() {
        let grid = CraftingGrid::default();
        assert_eq!(grid.kind(), GridKind::Player);
        assert!(grid.is_empty());
        assert_eq!(grid.preview(), None);
    }

    // -- Creative palette grants (S07-T02) --------------------------------

    #[test]
    fn palette_grant_hotbar_fills_empty_then_replaces_the_same_identity() {
        let mut inventory = Inventory::empty();

        assert_eq!(inventory.grant_hotbar(stick(7), 4), 0);
        assert_eq!(
            inventory.slot(0).map(|item| (item.id(), item.count())),
            Some((ItemId::Material(Material::Stick), 7))
        );

        assert_eq!(inventory.grant_hotbar(wheat(3), 4), 1);
        assert_eq!(
            inventory.slot(1).map(|item| (item.id(), item.count())),
            Some((ItemId::Material(Material::Wheat), 3))
        );

        // Fill the remaining seven slots so the next grant must take the
        // same-identity branch rather than an empty slot.
        for (offset, material) in [
            Material::Apple,
            Material::WheatSeeds,
            Material::Bread,
            Material::Leather,
            Material::RawPorkchop,
            Material::RawBeef,
            Material::RottenFlesh,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                inventory.grant_hotbar(item(ItemId::Material(material), 1), 8),
                2 + offset
            );
        }

        // A same-identity grant replaces the existing hotbar stack wholesale
        // (count 64 is the whole replacement, not a merge into 7).
        assert_eq!(inventory.grant_hotbar(stick(64), 8), 0);
        assert_eq!(
            inventory.slot(0).map(|item| (item.id(), item.count())),
            Some((ItemId::Material(Material::Stick), 64))
        );
        assert_eq!(
            inventory.slot(1).map(|item| (item.id(), item.count())),
            Some((ItemId::Material(Material::Wheat), 3)),
            "another identity's stack is untouched"
        );
    }

    #[test]
    fn palette_grant_hotbar_replaces_preferred_when_the_hotbar_is_full() {
        let mut inventory = Inventory::empty();
        for (slot, material) in [
            Material::Stick,
            Material::Apple,
            Material::Wheat,
            Material::WheatSeeds,
            Material::Bread,
            Material::Leather,
            Material::RawPorkchop,
            Material::RawBeef,
            Material::RottenFlesh,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                inventory.grant_hotbar(item(ItemId::Material(material), 1), 0),
                slot
            );
        }
        assert!(inventory.slots()[..HOTBAR_SLOTS]
            .iter()
            .all(Option::is_some));

        assert_eq!(inventory.grant_hotbar(block(BlockId::Stone, 999), 6), 6);
        assert_eq!(
            inventory.slot(6).map(|item| (item.id(), item.count())),
            Some((ItemId::Block(BlockId::Stone), 999))
        );

        // Out-of-range preferred values clamp to the last hotbar slot.
        assert_eq!(inventory.grant_hotbar(block(BlockId::Dirt, 999), 99), 8);
        assert_eq!(
            inventory.slot(8).map(|item| item.id()),
            Some(ItemId::Block(BlockId::Dirt))
        );
    }

    #[test]
    fn palette_grant_hotbar_never_touches_main_cursor_or_grid() {
        let mut slots: [Option<Item>; INVENTORY_SLOTS] = [None; INVENTORY_SLOTS];
        for slot in slots.iter_mut().take(HOTBAR_SLOTS) {
            *slot = Some(item(ItemId::Material(Material::Apple), 1));
        }
        for slot in slots.iter_mut().skip(HOTBAR_SLOTS) {
            *slot = Some(item(ItemId::Material(Material::Wheat), 2));
        }
        let cursor = item(ItemId::Material(Material::Bread), 3);
        let mut inventory = Inventory::from_parts(slots, Some(cursor));

        let mut grid = CraftingGrid::new(GridKind::Player);
        grid.set(0, Some(item(ItemId::Material(Material::Stick), 1)))
            .expect("player grid cell");

        let main_before: Vec<Option<Item>> = inventory.slots()[HOTBAR_SLOTS..].to_vec();
        let hotbar_before: Vec<Option<Item>> = inventory.slots()[..HOTBAR_SLOTS].to_vec();
        let cursor_before = inventory.cursor();
        let grid_before = grid.clone();

        assert_eq!(inventory.grant_hotbar(block(BlockId::Stone, 999), 3), 3);

        assert_eq!(
            inventory.slot(3).map(|item| (item.id(), item.count())),
            Some((ItemId::Block(BlockId::Stone), 999))
        );
        for (slot, before) in hotbar_before.iter().enumerate() {
            if slot == 3 {
                continue;
            }
            assert_eq!(inventory.slot(slot), *before);
        }
        assert_eq!(inventory.slots()[HOTBAR_SLOTS..].to_vec(), main_before);
        assert_eq!(inventory.cursor(), cursor_before);
        assert_eq!(grid, grid_before, "the crafting grid is never touched");
    }
}
