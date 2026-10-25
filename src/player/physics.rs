use glam::Vec3;

use crate::player::inventory::{
    CollectOutcome, CraftingGrid, GridKind, Inventory, InventoryError, Item, ItemStack, ToolWear,
    HOTBAR_SLOTS,
};
use crate::world::block::BlockId;

pub const PLAYER_HALF_WIDTH: f32 = 0.3;
pub const PLAYER_HEIGHT: f32 = 1.8;
pub const PLAYER_EYE_HEIGHT: f32 = 1.62;

#[derive(Copy, Clone, Debug)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    #[inline]
    pub fn from_feet(feet: Vec3) -> Self {
        Self {
            min: Vec3::new(
                feet.x - PLAYER_HALF_WIDTH,
                feet.y,
                feet.z - PLAYER_HALF_WIDTH,
            ),
            max: Vec3::new(
                feet.x + PLAYER_HALF_WIDTH,
                feet.y + PLAYER_HEIGHT,
                feet.z + PLAYER_HALF_WIDTH,
            ),
        }
    }

    #[inline]
    pub fn intersects_block(&self, bx: i32, by: i32, bz: i32) -> bool {
        let eps = 1e-4;
        self.max.x > bx as f32 + eps
            && self.min.x < (bx + 1) as f32 - eps
            && self.max.y > by as f32 + eps
            && self.min.y < (by + 1) as f32 - eps
            && self.max.z > bz as f32 + eps
            && self.min.z < (bz + 1) as f32 - eps
    }
}

#[derive(Clone, Debug, Default)]
pub struct InputState {
    pub forward: bool,
    pub backward: bool,
    pub left: bool,
    pub right: bool,
    pub jump_or_up: bool,
    pub crouch_or_down: bool,
    pub sprint: bool,
}

impl InputState {
    #[inline]
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Debug)]
pub struct Player {
    pub position: Vec3,
    pub velocity: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    pub fly_mode: bool,
    /// Selected hotbar slot, always in `0..HOTBAR_SLOTS`. Private so the only
    /// ways to change it are the bounds-checked methods below; session restore
    /// cannot publish an out-of-range selection.
    selected_slot: usize,
    /// The one authoritative item aggregate (9 hotbar + 27 main + cursor).
    inventory: Inventory,
    /// Unfinished 2x2 inventory crafting grid carried with the player.
    player_grid: CraftingGrid,
    /// Unfinished 4x4 crafting-table grid carried with the player.
    table_grid: CraftingGrid,
}

fn collides_with_world<F: Fn(i32, i32, i32) -> BlockId>(aabb: Aabb, get_block: &F) -> bool {
    let eps = 1e-4;
    let min_x = (aabb.min.x + eps).floor() as i32;
    let max_x = (aabb.max.x - eps).floor() as i32;
    let min_y = (aabb.min.y + eps).floor() as i32;
    let max_y = (aabb.max.y - eps).floor() as i32;
    let min_z = (aabb.min.z + eps).floor() as i32;
    let max_z = (aabb.max.z - eps).floor() as i32;

    for y in min_y..=max_y {
        for z in min_z..=max_z {
            for x in min_x..=max_x {
                if get_block(x, y, z).is_solid() {
                    return true;
                }
            }
        }
    }
    false
}

impl Player {
    pub fn new(position: Vec3) -> Self {
        Self {
            position,
            velocity: Vec3::ZERO,
            yaw: -std::f32::consts::FRAC_PI_4,
            pitch: -0.25,
            on_ground: false,
            fly_mode: true, // Start in fly mode so large worlds can be explored immediately; toggle with F
            selected_slot: 0,
            inventory: Inventory::empty(),
            player_grid: CraftingGrid::new(GridKind::Player),
            table_grid: CraftingGrid::new(GridKind::Table),
        }
    }

    /// Rebuilds a player from a persisted aggregate (session restore).
    ///
    /// The inventory is already validated by its own constructors; the grids
    /// are kind-checked here so a mismatched persisted grid safely defaults to
    /// the empty grid of the position's kind instead of putting 4x4 contents
    /// into the 2x2 player grid.
    pub fn with_inventory(
        position: Vec3,
        inventory: Inventory,
        player_grid: CraftingGrid,
        table_grid: CraftingGrid,
    ) -> Self {
        let mut player = Self::new(position);
        player.inventory = inventory;
        player.player_grid = if player_grid.kind() == GridKind::Player {
            player_grid
        } else {
            CraftingGrid::new(GridKind::Player)
        };
        player.table_grid = if table_grid.kind() == GridKind::Table {
            table_grid
        } else {
            CraftingGrid::new(GridKind::Table)
        };
        player
    }

    /// Selected hotbar index, guaranteed to be in `0..HOTBAR_SLOTS`.
    pub const fn selected_slot(&self) -> usize {
        self.selected_slot
    }

    /// Selects a hotbar slot. Returns `false` (and keeps the previous slot)
    /// when `slot` is outside the nine-slot hotbar.
    pub fn select_slot(&mut self, slot: usize) -> bool {
        if slot >= HOTBAR_SLOTS {
            return false;
        }
        self.selected_slot = slot;
        true
    }

    /// Cycles the selection by `step`, wrapping inside the nine hotbar slots.
    pub fn select_relative(&mut self, step: isize) {
        let slots = HOTBAR_SLOTS as isize;
        self.selected_slot = ((self.selected_slot as isize + step).rem_euclid(slots)) as usize;
    }

    /// Restores a persisted selection, clamping into the hotbar. Used only at
    /// the save/load boundary; it never leaves `selected_slot` out of range.
    pub fn restore_selected_slot(&mut self, slot: usize) {
        self.selected_slot = slot.min(HOTBAR_SLOTS - 1);
    }

    /// Read-only view of the durable inventory aggregate.
    pub fn inventory(&self) -> &Inventory {
        &self.inventory
    }

    pub fn player_grid(&self) -> &CraftingGrid {
        &self.player_grid
    }

    pub fn table_grid(&self) -> &CraftingGrid {
        &self.table_grid
    }

    /// Legacy block-only read of a hotbar slot (`None` for tools/out of range).
    pub fn hotbar_stack(&self, slot: usize) -> Option<ItemStack> {
        self.inventory.hotbar_stack(slot)
    }

    /// The selected hotbar item, if any.
    pub fn selected_item(&self) -> Option<Item> {
        self.inventory.hotbar(self.selected_slot)
    }

    /// The selected hotbar item's block identity, for placement.
    pub fn selected_block(&self) -> Option<BlockId> {
        self.selected_item()?.block_stack().map(|stack| stack.block)
    }

    /// Collects one broken block into the inventory (hotbar first, then main).
    pub fn collect_block(&mut self, block: BlockId) -> Option<CollectOutcome> {
        self.inventory.collect_block(block, 1)
    }

    /// Consumes exactly one item from the selected hotbar slot.
    pub fn consume_selected(&mut self) -> Option<Item> {
        self.inventory.consume_one(self.selected_slot)
    }

    /// Spends exactly one durability point on the selected hotbar tool.
    ///
    /// This is the narrow forwarder the world-edit path uses so the inventory
    /// aggregate itself stays private. An empty slot, an out-of-range
    /// selection, or a non-tool is an [`InventoryError`] with no mutation, so
    /// a bare-hand break can never fabricate or consume a tool.
    pub fn wear_selected_tool(&mut self) -> Result<ToolWear, InventoryError> {
        self.inventory.use_tool(self.selected_slot)
    }

    #[inline]
    pub fn aabb(&self) -> Aabb {
        Aabb::from_feet(self.position)
    }

    #[inline]
    pub fn eye_position(&self) -> Vec3 {
        self.position + Vec3::new(0.0, PLAYER_EYE_HEIGHT, 0.0)
    }

    pub fn update<F: Fn(i32, i32, i32) -> BlockId>(
        &mut self,
        dt: f32,
        input: &InputState,
        get_block: F,
    ) {
        let dt = dt.clamp(0.0, 0.1);
        let (sy, cy) = self.yaw.sin_cos();
        let forward_flat = Vec3::new(cy, 0.0, sy).normalize_or_zero();
        let right_flat = Vec3::new(-sy, 0.0, cy).normalize_or_zero();

        let mut move_dir = Vec3::ZERO;
        if input.forward {
            move_dir += forward_flat;
        }
        if input.backward {
            move_dir -= forward_flat;
        }
        if input.right {
            move_dir += right_flat;
        }
        if input.left {
            move_dir -= right_flat;
        }
        if move_dir.length_squared() > 0.0 {
            move_dir = move_dir.normalize();
        }

        if self.fly_mode {
            let speed = if input.sprint { 48.0 } else { 18.0 };
            let mut vel = move_dir * speed;
            if input.jump_or_up {
                vel.y += speed * 0.75;
            }
            if input.crouch_or_down {
                vel.y -= speed * 0.75;
            }
            self.velocity = vel;
            self.position += self.velocity * dt;
            self.on_ground = false;
            return;
        }

        let walk_speed = if input.sprint || input.crouch_or_down {
            7.2
        } else {
            4.6
        };
        self.velocity.x = move_dir.x * walk_speed;
        self.velocity.z = move_dir.z * walk_speed;

        if self.on_ground && input.jump_or_up {
            self.velocity.y = 9.0;
            self.on_ground = false;
        }

        self.velocity.y = (self.velocity.y - 28.0 * dt).max(-45.0);

        // Sub-step physics dynamically so no single step exceeds 0.25 blocks
        let steps = ((self.velocity.length() * dt) / 0.25).ceil().max(4.0) as usize;
        let sub_dt = dt / steps as f32;
        for _ in 0..steps {
            // 1. Resolve Y
            let dy = self.velocity.y * sub_dt;
            if dy != 0.0 {
                let candidate_y = Vec3::new(self.position.x, self.position.y + dy, self.position.z);
                if collides_with_world(Aabb::from_feet(candidate_y), &get_block) {
                    if dy < 0.0 {
                        self.position.y = (self.position.y + dy).ceil();
                        self.on_ground = true;
                    }
                    self.velocity.y = 0.0;
                } else {
                    self.position.y += dy;
                    self.on_ground = false;
                }
            }

            // 2. Resolve X
            let dx = self.velocity.x * sub_dt;
            if dx != 0.0 {
                let candidate_x = Vec3::new(self.position.x + dx, self.position.y, self.position.z);
                if !collides_with_world(Aabb::from_feet(candidate_x), &get_block) {
                    self.position.x += dx;
                } else {
                    self.velocity.x = 0.0;
                }
            }

            // 3. Resolve Z
            let dz = self.velocity.z * sub_dt;
            if dz != 0.0 {
                let candidate_z = Vec3::new(self.position.x, self.position.y, self.position.z + dz);
                if !collides_with_world(Aabb::from_feet(candidate_z), &get_block) {
                    self.position.z += dz;
                } else {
                    self.velocity.z = 0.0;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cannot_place_block_overlapping_player_aabb() {
        let player = Player::new(glam::Vec3::new(0.5, 10.0, 0.5));
        let aabb = player.aabb();
        assert!(aabb.intersects_block(0, 10, 0));
        assert!(aabb.intersects_block(0, 11, 0));
        assert!(!aabb.intersects_block(0, 9, 0));
        assert!(!aabb.intersects_block(2, 10, 0));
    }

    #[test]
    fn test_player_lands_on_ground_without_falling_through() {
        let mut player = Player::new(Vec3::new(0.5, 5.2, 0.5));
        player.fly_mode = false;
        let input = InputState::default();
        for _ in 0..30 {
            player.update(1.0 / 60.0, &input, |_, y, _| {
                if y < 5 {
                    BlockId::Stone
                } else {
                    BlockId::Air
                }
            });
        }
        assert!(player.on_ground);
        assert!((player.position.y - 5.0).abs() < 1e-3);
    }

    #[test]
    fn test_fast_fall_does_not_embed_in_floor() {
        let mut player = Player::new(Vec3::new(0.5, 5.05, 0.5));
        player.fly_mode = false;
        player.velocity.y = -45.0;
        let input = InputState::default();
        player.update(0.1, &input, |_, y, _| {
            if y < 5 {
                BlockId::Stone
            } else {
                BlockId::Air
            }
        });
        assert!(player.on_ground);
        assert!((player.position.y - 5.0).abs() < 1e-3);
    }

    #[test]
    fn test_left_shift_sprints_in_walk_mode() {
        let mut p_walk = Player::new(Vec3::new(0.5, 5.0, 0.5));
        p_walk.fly_mode = false;
        p_walk.yaw = 0.0;
        let mut p_sprint = p_walk.clone();

        let walk_input = InputState {
            forward: true,
            ..Default::default()
        };
        let shift_sprint_input = InputState {
            forward: true,
            crouch_or_down: true, // Left Shift sets crouch_or_down
            ..Default::default()
        };

        p_walk.update(0.05, &walk_input, |_, y, _| {
            if y < 5 {
                BlockId::Stone
            } else {
                BlockId::Air
            }
        });
        p_sprint.update(0.05, &shift_sprint_input, |_, y, _| {
            if y < 5 {
                BlockId::Stone
            } else {
                BlockId::Air
            }
        });

        assert!(p_sprint.position.x > p_walk.position.x + 0.05);
    }

    #[test]
    fn player_starts_with_empty_inventory_grids_and_selection() {
        let player = Player::new(Vec3::new(0.5, 10.0, 0.5));
        assert_eq!(player.selected_slot(), 0);
        assert!(player.inventory().slots().iter().all(Option::is_none));
        assert!(player.inventory().cursor().is_none());
        assert!(player.player_grid().is_empty());
        assert!(player.table_grid().is_empty());
        assert_eq!(player.selected_item(), None);
        assert_eq!(player.selected_block(), None);
    }

    #[test]
    fn select_slot_rejects_out_of_range_without_changing_selection() {
        let mut player = Player::new(Vec3::ZERO);
        assert!(player.select_slot(4));
        assert_eq!(player.selected_slot(), 4);

        assert!(!player.select_slot(HOTBAR_SLOTS));
        assert!(!player.select_slot(usize::MAX));
        assert_eq!(
            player.selected_slot(),
            4,
            "a rejected selection leaves the previous slot intact"
        );
    }

    #[test]
    fn select_relative_wraps_within_the_nine_slot_hotbar() {
        let mut player = Player::new(Vec3::ZERO);
        player.select_relative(-1);
        assert_eq!(player.selected_slot(), HOTBAR_SLOTS - 1);
        player.select_relative(1);
        assert_eq!(player.selected_slot(), 0);
        player.select_relative(4);
        player.select_relative(-9);
        assert_eq!(player.selected_slot(), 4);
    }

    #[test]
    fn restore_selected_slot_clamps_into_the_hotbar() {
        let mut player = Player::new(Vec3::ZERO);
        player.restore_selected_slot(200);
        assert_eq!(player.selected_slot(), HOTBAR_SLOTS - 1);
        player.restore_selected_slot(3);
        assert_eq!(player.selected_slot(), 3);
    }

    #[test]
    fn collect_block_fills_the_hotbar_then_spills_into_main_storage() {
        let mut player = Player::new(Vec3::ZERO);
        let hotbar_blocks = [
            BlockId::Stone,
            BlockId::Dirt,
            BlockId::Grass,
            BlockId::Sand,
            BlockId::OakLog,
            BlockId::OakLeaves,
            BlockId::Brick,
            BlockId::CoalOre,
            BlockId::IronOre,
        ];
        for block in hotbar_blocks {
            let outcome = player.collect_block(block).expect("block collects");
            assert!(outcome.first_slot < HOTBAR_SLOTS);
            assert_eq!(outcome.accepted, 1);
        }
        assert!(
            player.inventory().main(0).is_none(),
            "nine distinct blocks fill exactly the hotbar"
        );
        assert!(
            player.collect_block(BlockId::Bedrock).is_none(),
            "uncarryable blocks are rejected without mutation"
        );
    }

    #[test]
    fn collect_block_spills_into_main_storage_when_the_hotbar_is_full() {
        let mut player = Player::new(Vec3::ZERO);
        for block in [
            BlockId::Stone,
            BlockId::Dirt,
            BlockId::Grass,
            BlockId::Sand,
            BlockId::OakLog,
            BlockId::OakLeaves,
            BlockId::Brick,
            BlockId::CoalOre,
            BlockId::IronOre,
        ] {
            player.collect_block(block).expect("block collects");
        }

        let outcome = player
            .collect_block(BlockId::CraftingTable)
            .expect("a tenth identity has room in main storage");
        assert_eq!(outcome.first_slot, HOTBAR_SLOTS);
        assert_eq!(
            player.inventory().main(0).map(|item| item.id()),
            Some(crate::player::inventory::ItemId::Block(
                BlockId::CraftingTable
            ))
        );
    }

    #[test]
    fn consume_selected_takes_exactly_one_and_clears_the_slot_at_zero() {
        let mut player = Player::new(Vec3::ZERO);
        player.collect_block(BlockId::Stone).expect("collect");
        player.collect_block(BlockId::Stone).expect("collect");
        player.select_slot(0);

        assert_eq!(player.selected_item().map(|item| item.count()), Some(2));
        let consumed = player.consume_selected().expect("one item consumed");
        assert_eq!(consumed.count(), 1);
        assert_eq!(player.selected_item().map(|item| item.count()), Some(1));

        let consumed = player.consume_selected().expect("second item consumed");
        assert_eq!(consumed.count(), 1);
        assert_eq!(player.selected_item(), None);
        assert_eq!(player.consume_selected(), None, "empty slot is a no-op");
    }

    #[test]
    fn wear_selected_tool_forwarder_targets_only_the_selected_slot() {
        use crate::player::inventory::{
            Inventory, InventoryError, Item, ToolKind, ToolTier, ToolWear, INVENTORY_SLOTS,
        };

        let mut player = Player::new(Vec3::ZERO);
        player
            .collect_block(BlockId::Stone)
            .expect("selected non-tool block");
        assert!(player.select_slot(0));

        assert_eq!(
            player.wear_selected_tool(),
            Err(InventoryError::NotATool),
            "a non-tool selection spends nothing"
        );
        assert_eq!(
            player.selected_item().map(|item| item.count()),
            Some(1),
            "the rejected wear left the block stack intact"
        );

        // A tool in a later hotbar slot can only be worn by selecting it.
        let mut slots = [None; INVENTORY_SLOTS];
        slots[3] =
            Some(Item::tool_with_durability(ToolKind::Axe, ToolTier::Wood, 3).expect("worn axe"));
        player = Player::with_inventory(
            Vec3::ZERO,
            Inventory::from_parts(slots, None),
            CraftingGrid::new(GridKind::Player),
            CraftingGrid::new(GridKind::Table),
        );
        assert!(player.select_slot(3));

        match player.wear_selected_tool() {
            Ok(ToolWear::Worn(item)) => assert_eq!(item.durability(), Some(2)),
            other => panic!("expected one point of wear, got {other:?}"),
        }
        assert_eq!(
            player.selected_item().and_then(|item| item.durability()),
            Some(2)
        );

        // Final durability breaks the tool through the same narrow forwarder.
        let mut slots = [None; INVENTORY_SLOTS];
        slots[3] =
            Some(Item::tool_with_durability(ToolKind::Axe, ToolTier::Wood, 1).expect("last use"));
        player = Player::with_inventory(
            Vec3::ZERO,
            Inventory::from_parts(slots, None),
            CraftingGrid::new(GridKind::Player),
            CraftingGrid::new(GridKind::Table),
        );
        assert!(player.select_slot(3));
        assert_eq!(player.wear_selected_tool(), Ok(ToolWear::Broke));
        assert_eq!(player.selected_item(), None, "a broken tool frees its slot");
        assert_eq!(
            player.wear_selected_tool(),
            Err(InventoryError::EmptySlot),
            "the cleared slot is an explicit no-op"
        );
    }

    #[test]
    fn with_inventory_preserves_cursor_tool_and_unfinished_grids() {
        use crate::player::inventory::{
            Item, ItemId, Material, ToolKind, ToolTier, INVENTORY_SLOTS,
        };

        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(Item::new(ItemId::Block(BlockId::Stone), 3).expect("stone"));
        slots[HOTBAR_SLOTS + 2] =
            Some(Item::new(ItemId::Material(Material::Stick), 12).expect("sticks"));
        let pickaxe =
            Item::tool_with_durability(ToolKind::Pickaxe, ToolTier::Iron, 137).expect("tool");
        let inventory = Inventory::from_parts(slots, Some(pickaxe));

        let mut player_grid = CraftingGrid::new(GridKind::Player);
        player_grid
            .set(
                0,
                Some(Item::new(ItemId::Material(Material::Stick), 1).expect("stick")),
            )
            .expect("grid cell");
        let mut table_grid = CraftingGrid::new(GridKind::Table);
        table_grid
            .set(
                8,
                Some(Item::new(ItemId::Block(BlockId::IronOre), 2).expect("ore")),
            )
            .expect("grid cell");

        let player = Player::with_inventory(
            Vec3::new(1.0, 2.0, 3.0),
            inventory.clone(),
            player_grid.clone(),
            table_grid.clone(),
        );
        assert_eq!(player.inventory(), &inventory);
        assert_eq!(player.player_grid(), &player_grid);
        assert_eq!(player.table_grid(), &table_grid);
        assert_eq!(
            player
                .inventory()
                .main(2)
                .map(|item| (item.count(), item.id())),
            Some((12, ItemId::Material(Material::Stick)))
        );
        assert_eq!(
            player
                .inventory()
                .cursor()
                .and_then(|item| item.durability()),
            Some(137),
            "tool durability survives the player boundary"
        );
    }

    #[test]
    fn with_inventory_defaults_mismatched_grid_kinds_safely() {
        let player = Player::with_inventory(
            Vec3::ZERO,
            Inventory::empty(),
            CraftingGrid::new(GridKind::Table),
            CraftingGrid::new(GridKind::Player),
        );
        assert_eq!(player.player_grid().kind(), GridKind::Player);
        assert_eq!(player.table_grid().kind(), GridKind::Table);
        assert!(player.player_grid().is_empty());
        assert!(player.table_grid().is_empty());
    }

    #[test]
    fn test_focus_loss_resets_input_state() {
        let mut input = InputState {
            forward: true,
            backward: true,
            left: true,
            right: true,
            jump_or_up: true,
            crouch_or_down: true,
            sprint: true,
        };
        input.reset();
        assert!(!input.forward && !input.backward && !input.left && !input.right);
        assert!(!input.jump_or_up && !input.crouch_or_down && !input.sprint);
    }
}
