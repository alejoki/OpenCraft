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

use crate::world::block::BlockId;

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
        if matches!(block, BlockId::Air | BlockId::Bedrock) || count == 0 || count > MAX_STACK_COUNT
        {
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
        if matches!(block, BlockId::Air | BlockId::Bedrock) || amount == 0 {
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

/// Non-block materials produced by crafting.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Material {
    Stick,
}

impl Material {
    pub const fn name(self) -> &'static str {
        match self {
            Material::Stick => "Stick",
        }
    }
}

/// Tool shape. Tools are always non-stackable and carry durability.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ToolKind {
    Pickaxe,
    Axe,
    Shovel,
}

impl ToolKind {
    pub const fn name(self) -> &'static str {
        match self {
            ToolKind::Pickaxe => "Pickaxe",
            ToolKind::Axe => "Axe",
            ToolKind::Shovel => "Shovel",
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
    Tool { kind: ToolKind, tier: ToolTier },
}

impl ItemId {
    /// Maximum count this identity may occupy in one slot.
    pub const fn stack_limit(self) -> u16 {
        match self {
            ItemId::Block(_) => MAX_STACK_COUNT,
            ItemId::Material(_) => MATERIAL_STACK_LIMIT,
            ItemId::Tool { .. } => 1,
        }
    }

    pub const fn is_tool(self) -> bool {
        matches!(self, ItemId::Tool { .. })
    }

    pub const fn tool(self) -> Option<(ToolKind, ToolTier)> {
        match self {
            ItemId::Tool { kind, tier } => Some((kind, tier)),
            _ => None,
        }
    }

    /// Human-readable label for HUD/log surfaces.
    pub fn name(self) -> &'static str {
        match self {
            ItemId::Block(block) => block.name(),
            ItemId::Material(material) => material.name(),
            ItemId::Tool { kind, tier } => match (tier, kind) {
                (ToolTier::Wood, ToolKind::Pickaxe) => "Wood Pickaxe",
                (ToolTier::Wood, ToolKind::Axe) => "Wood Axe",
                (ToolTier::Wood, ToolKind::Shovel) => "Wood Shovel",
                (ToolTier::Stone, ToolKind::Pickaxe) => "Stone Pickaxe",
                (ToolTier::Stone, ToolKind::Axe) => "Stone Axe",
                (ToolTier::Stone, ToolKind::Shovel) => "Stone Shovel",
                (ToolTier::Iron, ToolKind::Pickaxe) => "Iron Pickaxe",
                (ToolTier::Iron, ToolKind::Axe) => "Iron Axe",
                (ToolTier::Iron, ToolKind::Shovel) => "Iron Shovel",
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
    /// Air and bedrock cannot be carried.
    UncarryableBlock,
    /// Tools are non-stackable and must have count one.
    StackableTool,
    /// Durability was applied to, or requested from, a non-tool.
    NotATool,
    /// Tool durability must be `1..=tier.max_durability()`.
    InvalidDurability,
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
}

impl Item {
    /// Builds a fully-repaired instance of `id` with `count` items.
    ///
    /// Blocks reject air/bedrock, materials enforce
    /// [`MATERIAL_STACK_LIMIT`], blocks enforce [`MAX_STACK_COUNT`], and tools
    /// must be count one (they start at full durability).
    pub fn new(id: ItemId, count: u16) -> Result<Self, ItemError> {
        match id {
            ItemId::Block(BlockId::Air | BlockId::Bedrock) => Err(ItemError::UncarryableBlock),
            ItemId::Block(_) => Self::stackable(id, count, MAX_STACK_COUNT),
            ItemId::Material(_) => Self::stackable(id, count, MATERIAL_STACK_LIMIT),
            ItemId::Tool { tier, .. } => {
                if count != 1 {
                    return Err(ItemError::StackableTool);
                }
                Ok(Self {
                    id,
                    count: 1,
                    durability: Some(tier.max_durability()),
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
        if durability == 0 || durability > tier.max_durability() {
            return Err(ItemError::InvalidDurability);
        }
        Ok(Self {
            id: ItemId::Tool { kind, tier },
            count: 1,
            durability: Some(durability),
        })
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
        self.id.tool().map(|(_, tier)| tier.max_durability())
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

/// Errors from inventory mutation. Every error leaves the inventory exactly
/// as it was before the call.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InventoryError {
    OutOfBounds,
    EmptySlot,
    EmptyCursor,
    /// Cursor holds a different identity and cannot receive the slot stack.
    CursorOccupied,
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

    /// Best-effort block collection in hotbar-then-main order.
    ///
    /// Unlike [`Inventory::insert`], this matches world pickup semantics: it
    /// accepts what fits and reports how much it took so the caller can keep
    /// the remainder. Returns `None` (no mutation) when nothing was accepted
    /// or the block cannot be carried.
    pub fn collect_block(&mut self, block: BlockId, amount: u16) -> Option<CollectOutcome> {
        if matches!(block, BlockId::Air | BlockId::Bedrock) || amount == 0 {
            return None;
        }

        let id = ItemId::Block(block);
        let limit = MAX_STACK_COUNT;
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
                *slot = Item::new(id, moved).ok();
                remaining -= moved;
                if first_slot.is_none() {
                    first_slot = Some(index);
                }
            }
        }

        first_slot.map(|first_slot| CollectOutcome {
            first_slot,
            accepted: amount - remaining,
        })
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
    WoodPickaxe,
    WoodAxe,
    WoodShovel,
    StonePickaxe,
    StoneAxe,
    StoneShovel,
    IronPickaxe,
    IronAxe,
    IronShovel,
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
    /// The destination inventory cannot hold the recipe output right now.
    DestinationFull,
}

/// Shaped crafting recipe. Each non-space pattern character is one item
/// identity and one consumed grid cell; the pattern may sit anywhere in the
/// grid as long as its trimmed bounding box matches.
#[derive(Copy, Clone)]
struct Recipe {
    id: RecipeId,
    pattern: &'static [&'static str],
    output: ItemId,
    output_count: u16,
}

impl Recipe {
    fn output_item(&self) -> Item {
        Item::new(self.output, self.output_count).expect("static recipes are valid")
    }
}

/// Shaped recipe table.
///
/// The material chain is grounded in what the world actually yields today:
/// oak logs become planks, planks become sticks and the crafting table, and
/// the three tool tiers use planks, cobblestone, and mined iron ore as their
/// head material. Adding smelting later can swap the iron head identity
/// without changing the shaped-recipe machinery.
const RECIPES: &[Recipe] = &[
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
];

/// Pattern character to item identity. Unknown characters never match.
fn pattern_item(byte: u8) -> Option<ItemId> {
    match byte {
        b'L' => Some(ItemId::Block(BlockId::OakLog)),
        b'P' => Some(ItemId::Block(BlockId::OakPlanks)),
        b'C' => Some(ItemId::Block(BlockId::Cobblestone)),
        b'I' => Some(ItemId::Block(BlockId::IronOre)),
        b'S' => Some(ItemId::Material(Material::Stick)),
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

fn pattern_bounds(pattern: &'static [&'static str]) -> Option<Bounds> {
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

fn matches_pattern(grid: &CraftingGrid, grid_bounds: Bounds, recipe: &Recipe) -> bool {
    let Some(pattern_bounds) = pattern_bounds(recipe.pattern) else {
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
                let Some(expected_id) = pattern_item(expected) else {
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

fn recipe_for(grid: &CraftingGrid) -> Option<&'static Recipe> {
    let grid_bounds = grid_bounds(grid)?;
    RECIPES
        .iter()
        .find(|recipe| matches_pattern(grid, grid_bounds, recipe))
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

    /// Evaluates the current grid contents. Never cached: the result is a pure
    /// function of the cells at call time.
    pub fn preview(&self) -> Option<RecipePreview> {
        recipe_for(self).map(|recipe| RecipePreview {
            recipe: recipe.id,
            output: recipe.output,
            output_count: recipe.output_count,
        })
    }

    /// Crafts one result: revalidate the recipe and destination, insert the
    /// output, then consume exactly one item from each occupied cell.
    ///
    /// Any error (no recipe, full destination) leaves both the grid and the
    /// inventory untouched. The recipe is always re-derived from the current
    /// cells, so a stale caller-held preview can never take a result.
    pub fn take_result(&mut self, inventory: &mut Inventory) -> Result<Item, CraftError> {
        let recipe = recipe_for(self).ok_or(CraftError::NoRecipe)?;
        let output = recipe.output_item();
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

    fn tool(kind: ToolKind, tier: ToolTier) -> Item {
        item(ItemId::Tool { kind, tier }, 1)
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
        assert_eq!(inventory.collect_block(BlockId::Stone, 0), None);
        assert_eq!(inventory, Inventory::empty());
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
    fn every_tool_recipe_is_shaped_and_produces_a_full_durability_instance() {
        let cases: [(RecipeId, ToolKind, ToolTier, &[&str]); 9] = [
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
    fn recipe_table_patterns_are_unambiguous() {
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

    #[test]
    fn default_crafting_grid_is_an_empty_player_grid() {
        let grid = CraftingGrid::default();
        assert_eq!(grid.kind(), GridKind::Player);
        assert!(grid.is_empty());
        assert_eq!(grid.preview(), None);
    }
}
