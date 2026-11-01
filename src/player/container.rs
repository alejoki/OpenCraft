//! Pure container domain: a 27-slot chest and its cursor transfers.
//!
//! [`Container`] wraps the durable [`ChestSlots`] storage so chest contents
//! can only be mutated through conservation-checked operations. The cursor
//! lives in [`Inventory`] (players own exactly one hand stack shared by every
//! screen), so [`Container::click_slot`] is the single seam between chest
//! storage and the player aggregate.
//!
//! Every transfer computes its fallible values before touching either side:
//! a rejected click leaves the chest slot and the cursor bit-identical, which
//! is what lets the UI report an error and retry without losing or
//! duplicating an item.

use crate::player::inventory::{ChestSlots, Inventory, InventoryError, Item, CHEST_SLOTS};

/// 27-slot chest storage with whole-stack cursor semantics.
///
/// The slots are stored in display order (three rows of nine). An empty
/// `Container` is the canonical "never touched"/"empty chest" value; world
/// persistence decides whether a record exists at all.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Container {
    slots: ChestSlots,
}

impl Container {
    /// All 27 slots empty.
    pub const fn empty() -> Self {
        Self {
            slots: [None; CHEST_SLOTS],
        }
    }

    /// Rebuilds a container from already-validated slots.
    ///
    /// Items can only exist through [`Item`]'s checked constructors, so this
    /// cannot introduce forged values. Used by persistence decode and tests.
    pub fn from_slots(slots: ChestSlots) -> Self {
        Self { slots }
    }

    /// Read-only view of all 27 slots in display order.
    pub fn slots(&self) -> &ChestSlots {
        &self.slots
    }

    /// Bounds-checked slot read. Out-of-range indexes are `None`.
    pub fn slot(&self, index: usize) -> Option<Item> {
        self.slots.get(index).copied().flatten()
    }

    /// Whether every slot is empty.
    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }

    /// Non-empty stacks in slot order, as a flat item list.
    ///
    /// This is the break-preflight view: the caller adds the chest's own
    /// dropped item and asks the inventory whether the whole batch fits.
    pub fn contents(&self) -> Vec<Item> {
        self.slots.iter().flatten().copied().collect()
    }

    /// Whole-stack left-click between a chest slot and the cursor.
    ///
    /// - Empty cursor + occupied slot: the slot stack moves onto the cursor.
    /// - Occupied cursor + empty slot: the cursor stack moves into the slot.
    /// - Matching stackable items: merge up to the stack limit; any remainder
    ///   stays on the cursor. A full matching slot is [`InventoryError::SlotFull`].
    /// - Different identity or a tool: swap the slot and cursor stacks.
    /// - Empty/empty: inert no-op.
    /// - Out-of-range index: [`InventoryError::OutOfBounds`] before any read.
    ///
    /// Every error leaves the chest slot and cursor exactly as they were; the
    /// combined item count is conserved on every success as well.
    pub fn click_slot(
        &mut self,
        inventory: &mut Inventory,
        index: usize,
    ) -> Result<(), InventoryError> {
        let slot_item = self
            .slots
            .get(index)
            .copied()
            .ok_or(InventoryError::OutOfBounds)?;
        let cursor = inventory.cursor();

        match (cursor, slot_item) {
            (None, None) => Ok(()),
            (None, Some(item)) => {
                // The cursor is empty, so `put_cursor` cannot fail. Put first
                // and only then clear the slot: a rejected put leaves the
                // chest untouched.
                inventory.put_cursor(item)?;
                self.slots[index] = None;
                Ok(())
            }
            (Some(_), None) => {
                // The cursor is occupied, so `take_cursor` cannot fail. The
                // slot stays empty until the hand value is in the local.
                let item = inventory.take_cursor()?;
                self.slots[index] = Some(item);
                Ok(())
            }
            (Some(cursor_item), Some(slot_item)) => {
                if cursor_item.mergeable_with(&slot_item) {
                    self.merge_into_slot(inventory, index, cursor_item, slot_item)
                } else {
                    self.swap_with_cursor(inventory, index, slot_item)
                }
            }
        }
    }

    /// Merges the cursor stack into an occupied, compatible slot.
    ///
    /// The whole computation happens before the first mutation: a full slot
    /// is an error, and the merged slot/cursor values are built with the same
    /// checked `with_count` primitive the inventory uses.
    fn merge_into_slot(
        &mut self,
        inventory: &mut Inventory,
        index: usize,
        cursor_item: Item,
        slot_item: Item,
    ) -> Result<(), InventoryError> {
        let limit = cursor_item.id().stack_limit();
        if slot_item.count() >= limit {
            return Err(InventoryError::SlotFull);
        }

        let moved = cursor_item.count().min(limit - slot_item.count());
        let new_slot = slot_item
            .with_count(slot_item.count() + moved)
            .map_err(|_| InventoryError::SlotFull)?;
        let new_cursor = if moved == cursor_item.count() {
            None
        } else {
            Some(
                cursor_item
                    .with_count(cursor_item.count() - moved)
                    .map_err(|_| InventoryError::SlotFull)?,
            )
        };

        // The cursor is occupied, so `take_cursor` cannot fail; after taking,
        // `put_cursor` (only when a remainder exists) cannot fail either.
        let hand = inventory.take_cursor()?;
        debug_assert_eq!(hand, cursor_item);
        self.slots[index] = Some(new_slot);
        if let Some(remainder) = new_cursor {
            inventory
                .put_cursor(remainder)
                .expect("the cursor was just cleared");
        }
        Ok(())
    }

    /// Swaps an occupied chest slot with an occupied cursor.
    fn swap_with_cursor(
        &mut self,
        inventory: &mut Inventory,
        index: usize,
        slot_item: Item,
    ) -> Result<(), InventoryError> {
        let hand = inventory.take_cursor()?;
        self.slots[index] = Some(hand);
        inventory
            .put_cursor(slot_item)
            .expect("the cursor was just cleared");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::inventory::{
        InventoryError, ItemId, Material, ToolKind, ToolTier, MAX_STACK_COUNT,
    };
    use crate::world::block::BlockId;

    fn item(id: ItemId, count: u16) -> Item {
        Item::new(id, count).expect("valid test item")
    }

    fn block(block: BlockId, count: u16) -> Item {
        item(ItemId::Block(block), count)
    }

    fn stick(count: u16) -> Item {
        item(ItemId::Material(Material::Stick), count)
    }

    fn pickaxe() -> Item {
        item(
            ItemId::Tool {
                kind: ToolKind::Pickaxe,
                tier: ToolTier::Iron,
            },
            1,
        )
    }

    fn stone(count: u16) -> Item {
        block(BlockId::Stone, count)
    }

    fn chest_with<const N: usize>(entries: [(usize, Item); N]) -> Container {
        let mut slots = [None; CHEST_SLOTS];
        for (index, item) in entries {
            slots[index] = Some(item);
        }
        Container::from_slots(slots)
    }

    /// Total item units across the chest and the cursor. A click that changes
    /// this number has duplicated or destroyed items.
    fn units(container: &Container, inventory: &Inventory) -> u32 {
        let stored = container
            .slots()
            .iter()
            .flatten()
            .map(|item| u32::from(item.count()))
            .sum::<u32>();
        stored + inventory.cursor().map_or(0, |item| u32::from(item.count()))
    }

    #[test]
    fn empty_container_has_twenty_seven_empty_slots() {
        let container = Container::empty();
        assert_eq!(container.slots().len(), CHEST_SLOTS);
        assert_eq!(CHEST_SLOTS, 27);
        assert!(container.is_empty());
        assert!(container.slots().iter().all(Option::is_none));
        assert_eq!(container.slot(0), None);
        assert_eq!(container.slot(CHEST_SLOTS - 1), None);
        assert_eq!(container.slot(CHEST_SLOTS), None);
        assert_eq!(container.contents(), Vec::new());
        assert_eq!(container, Container::default());
    }

    #[test]
    fn from_slots_keeps_positions_and_contents_in_slot_order() {
        let table = block(BlockId::CraftingTable, 1);
        let apple = item(ItemId::Material(Material::Apple), 1);
        let bread = item(ItemId::Material(Material::Bread), 2);
        let container = chest_with([(0, table), (13, apple), (26, bread)]);

        assert!(!container.is_empty());
        assert_eq!(container.slot(0), Some(table));
        assert_eq!(container.slot(13), Some(apple));
        assert_eq!(container.slot(26), Some(bread));
        assert_eq!(container.slot(1), None);
        assert_eq!(container.slot(CHEST_SLOTS), None);
        assert_eq!(
            container.contents(),
            vec![table, apple, bread],
            "contents follow slot order and skip empties"
        );
        assert_eq!(Container::from_slots(*container.slots()), container);
    }

    #[test]
    fn two_empty_sides_are_an_inert_no_op() {
        let mut container = Container::empty();
        let mut inventory = Inventory::empty();
        let before = (container.clone(), inventory.clone());

        container
            .click_slot(&mut inventory, 0)
            .expect("empty/empty is fine");
        assert_eq!((container, inventory), before);
    }

    #[test]
    fn empty_cursor_picks_up_the_whole_chest_stack() {
        let mut container = chest_with([(4, stone(5))]);
        let mut inventory = Inventory::empty();

        container
            .click_slot(&mut inventory, 4)
            .expect("pick up the whole stack");
        assert_eq!(container.slot(4), None);
        assert_eq!(inventory.cursor(), Some(stone(5)));
        assert_eq!(units(&container, &inventory), 5);
    }

    #[test]
    fn occupied_cursor_puts_the_whole_stack_into_an_empty_slot() {
        let mut container = chest_with([(4, stone(5))]);
        let mut inventory = Inventory::empty();
        container
            .click_slot(&mut inventory, 4)
            .expect("pick up the whole stack");

        container
            .click_slot(&mut inventory, 20)
            .expect("put the stack down elsewhere");
        assert_eq!(container.slot(20), Some(stone(5)));
        assert_eq!(container.slot(4), None);
        assert_eq!(inventory.cursor(), None);
        assert_eq!(units(&container, &inventory), 5);
    }

    #[test]
    fn matching_stacks_merge_up_to_the_limit_and_leave_the_remainder_on_the_cursor() {
        let mut container = chest_with([(0, stick(5))]);
        let mut inventory = Inventory::empty();
        inventory
            .put_cursor(stick(64))
            .expect("cursor holds a full stack");

        container
            .click_slot(&mut inventory, 0)
            .expect("merge into the matching stack");
        assert_eq!(
            container.slot(0),
            Some(stick(64)),
            "the chest slot fills to the material limit"
        );
        assert_eq!(
            inventory.cursor(),
            Some(stick(5)),
            "the remainder stays on the cursor"
        );
        assert_eq!(units(&container, &inventory), 69);
    }

    #[test]
    fn matching_stacks_merge_completely_and_empty_the_cursor() {
        let mut container = chest_with([(0, stone(MAX_STACK_COUNT - 3))]);
        let mut inventory = Inventory::empty();
        inventory
            .put_cursor(stone(3))
            .expect("cursor holds three stone");

        container
            .click_slot(&mut inventory, 0)
            .expect("merge the last three units");
        assert_eq!(container.slot(0), Some(stone(MAX_STACK_COUNT)));
        assert_eq!(inventory.cursor(), None);
        assert_eq!(units(&container, &inventory), u32::from(MAX_STACK_COUNT));
    }

    #[test]
    fn different_identities_swap_the_slot_and_cursor() {
        let mut container = chest_with([(6, stone(2))]);
        let mut inventory = Inventory::empty();
        inventory.put_cursor(stick(3)).expect("cursor holds sticks");

        container
            .click_slot(&mut inventory, 6)
            .expect("different identities swap");
        assert_eq!(container.slot(6), Some(stick(3)));
        assert_eq!(inventory.cursor(), Some(stone(2)));
        assert_eq!(units(&container, &inventory), 5);
    }

    #[test]
    fn tools_never_merge_and_always_swap() {
        let mut container = chest_with([(2, pickaxe())]);
        let mut inventory = Inventory::empty();
        inventory.put_cursor(stone(3)).expect("cursor holds stone");

        container
            .click_slot(&mut inventory, 2)
            .expect("tool swaps with a stack");
        assert_eq!(container.slot(2), Some(stone(3)));
        assert_eq!(inventory.cursor(), Some(pickaxe()));
        assert_eq!(units(&container, &inventory), 4);

        // Same identity, different durability: tools still swap rather than
        // merge, because tool slots are non-stackable.
        let worn =
            Item::tool_with_durability(ToolKind::Pickaxe, ToolTier::Iron, 5).expect("worn pickaxe");
        let mut second = chest_with([(3, worn)]);
        container
            .click_slot(&mut inventory, 2)
            .expect("swap the stone and pickaxe again");
        assert_eq!(inventory.cursor(), Some(stone(3)));
        second
            .click_slot(&mut inventory, 3)
            .expect("same-identity tools swap");
        assert_eq!(second.slot(3), Some(stone(3)));
        assert_eq!(
            inventory.cursor(),
            Some(worn),
            "the worn instance comes back onto the cursor"
        );
        assert_eq!(units(&second, &inventory), 4);
    }

    #[test]
    fn a_full_matching_slot_is_rejected_without_mutation() {
        let mut container = chest_with([(0, stone(MAX_STACK_COUNT))]);
        let mut inventory = Inventory::empty();
        inventory
            .put_cursor(stone(1))
            .expect("cursor holds one stone");
        let before = (container.clone(), inventory.clone());

        assert_eq!(
            container.click_slot(&mut inventory, 0),
            Err(InventoryError::SlotFull)
        );
        assert_eq!((container, inventory), before);
    }

    #[test]
    fn out_of_bounds_is_rejected_before_anything_moves() {
        let mut container = chest_with([(0, stone(4))]);
        let mut inventory = Inventory::empty();
        let empty_before = (container.clone(), inventory.clone());

        assert_eq!(
            container.click_slot(&mut inventory, CHEST_SLOTS),
            Err(InventoryError::OutOfBounds)
        );
        assert_eq!(
            container.click_slot(&mut inventory, usize::MAX),
            Err(InventoryError::OutOfBounds)
        );
        assert_eq!(container, empty_before.0);
        assert_eq!(inventory, empty_before.1);

        inventory.put_cursor(stick(2)).expect("cursor holds sticks");
        let occupied_before = (container.clone(), inventory.clone());
        assert_eq!(
            container.clone().click_slot(&mut inventory, CHEST_SLOTS),
            Err(InventoryError::OutOfBounds)
        );
        assert_eq!(container, occupied_before.0);
        assert_eq!(inventory, occupied_before.1);
    }

    #[test]
    fn every_click_outcome_conserves_item_counts() {
        let mut container = chest_with([(0, stone(10)), (1, stick(2)), (2, pickaxe())]);
        let mut inventory = Inventory::empty();
        let starting_units = units(&container, &inventory);

        // Each success conserves the combined total.
        for index in [0, 1, 0, 0, 2, 0, 2, 1] {
            let before = units(&container, &inventory);
            container
                .click_slot(&mut inventory, index)
                .unwrap_or_else(|error| panic!("click {index} failed: {error:?}"));
            assert_eq!(
                units(&container, &inventory),
                before,
                "successful click {index} must conserve item units"
            );
        }

        // Failure paths conserve the total AND leave both sides bit-identical.
        let full_before = chest_with([(0, stone(MAX_STACK_COUNT)), (1, stick(64))]);
        let mut full = full_before.clone();
        let mut hand = Inventory::empty();
        hand.put_cursor(stone(1)).expect("cursor holds one stone");
        let state_before = (full.clone(), hand.clone());
        assert_eq!(full.click_slot(&mut hand, 0), Err(InventoryError::SlotFull));
        assert_eq!((full, hand), state_before);

        assert_eq!(
            units(&container, &inventory),
            starting_units,
            "the whole sequence conserves the starting item units"
        );
    }
}
