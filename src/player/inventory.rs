//! Pure hotbar inventory model: finite id+count stacks.
//!
//! This module deliberately has no winit/wgpu dependency so the collection,
//! merge, and consume rules can be exercised headlessly. Persistence and HUD
//! wiring live in other layers.

use crate::world::block::BlockId;

/// Number of hotbar slots.
pub const HOTBAR_SLOTS: usize = 9;

/// Provisional per-slot count cap. M006 formalizes per-item stack sizes.
pub const MAX_STACK_COUNT: u16 = 999;

/// A single hotbar stack: a block id and a finite count.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ItemStack {
    pub block: BlockId,
    pub count: u16,
}

impl ItemStack {
    /// Builds a stack, rejecting blocks that cannot be carried and counts
    /// outside `1..=MAX_STACK_COUNT`.
    pub fn new(block: BlockId, count: u16) -> Option<Self> {
        if matches!(block, BlockId::Air | BlockId::Bedrock)
            || count == 0
            || count > MAX_STACK_COUNT
        {
            return None;
        }
        Some(Self { block, count })
    }
}

/// Fixed-size hotbar of optional stacks.
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
