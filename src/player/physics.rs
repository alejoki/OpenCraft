use glam::Vec3;

use crate::player::inventory::{
    CollectOutcome, CraftError, CraftingGrid, GridKind, Inventory, InventoryError, Item, ItemStack,
    RecipePreview, ToolWear, HOTBAR_SLOTS,
};
use crate::world::block::BlockId;

/// Errors from the narrow inventory-screen command façade.
///
/// Every variant is a rejected command whose pre-state (slots, cursor, both
/// grids) is unchanged, so the UI can report the failure and retry without
/// ever losing or duplicating an item.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InventoryCommandError {
    /// A slot click was rejected by the inventory aggregate.
    Inventory(InventoryError),
    /// A grid/result command was rejected by the crafting domain.
    Craft(CraftError),
    /// A table target was used without a live placed table (adapter layer).
    TableUnavailable,
}

impl From<InventoryError> for InventoryCommandError {
    fn from(error: InventoryError) -> Self {
        Self::Inventory(error)
    }
}

impl From<CraftError> for InventoryCommandError {
    fn from(error: CraftError) -> Self {
        Self::Craft(error)
    }
}

pub const PLAYER_HALF_WIDTH: f32 = 0.3;
pub const PLAYER_HEIGHT: f32 = 1.8;
pub const PLAYER_EYE_HEIGHT: f32 = 1.62;

/// Upward acceleration applied to a submerged player (net positive against the
/// 28.0 m/s^2 gravity in `Player::update`, so a released body rises).
pub const SWIM_BUOYANCY_ACCEL: f32 = 41.0;
/// Per-second exponential drag on vertical velocity while submerged. With
/// `SWIM_BUOYANCY_ACCEL - gravity` this bounds the natural float at roughly
/// 2.2 m/s: gentle, never a teleport.
pub const SWIM_VERTICAL_DRAG: f32 = 6.0;
/// Upward speed while `jump_or_up` is held underwater. Deliberately below the
/// 9.0 m/s land jump so swimming up reads as a steady rise, not a leap.
pub const SWIM_RISE_SPEED: f32 = 4.0;
/// Terminal sink speed underwater, so entering from a long fall never slams the
/// body into the seabed at the 45 m/s dry-land terminal velocity.
pub const SWIM_MAX_SINK_SPEED: f32 = 6.0;
/// Horizontal walk-speed multiplier while submerged: 4.6 * 0.55 = 2.53 m/s, so
/// swimming is always slower than the 4.6 m/s land walk (and the 7.2 sprint).
pub const SWIM_WALK_SPEED_MULT: f32 = 0.55;

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

    // -----------------------------------------------------------------------
    // Inventory-screen command façade
    //
    // These are the only mutation entry points a UI click may use. They never
    // compute stack arithmetic: every transfer goes through the aggregate's
    // conservation-checked primitives, whole-stack semantics stay in the
    // domain, and a rejected command leaves slots, cursor, and both grids in
    // their pre-command state.
    // -----------------------------------------------------------------------

    /// Moves a whole stack between one of the 36 inventory slots and the
    /// cursor, using the aggregate's merge/swap rules.
    ///
    /// An empty cursor picks the slot stack up ([`Inventory::pick_up`]); an
    /// occupied cursor puts/down-merges/swaps via [`Inventory::put_down`].
    /// Full targets and out-of-bounds indexes are rejected without mutation.
    pub fn click_inventory_slot(&mut self, index: usize) -> Result<(), InventoryError> {
        if self.inventory.cursor().is_some() {
            self.inventory.put_down(index)
        } else {
            self.inventory.pick_up(index)
        }
    }

    /// Right-click slot command: split-half or place-one.
    ///
    /// An empty cursor picks up half the slot stack, rounded up
    /// ([`Inventory::pick_up_half`]); an occupied cursor puts exactly one unit
    /// into the slot ([`Inventory::place_one`]). Rejected commands leave the
    /// slot and cursor unchanged.
    pub fn click_inventory_slot_right(&mut self, index: usize) -> Result<(), InventoryError> {
        if self.inventory.cursor().is_some() {
            self.inventory.place_one(index)
        } else {
            self.inventory.pick_up_half(index)
        }
    }

    /// Moves a whole stack between one crafting-grid cell and the cursor.
    ///
    /// `kind` selects the grid; a player-grid command can never read or write
    /// the table grid (and vice versa). Four transitions exist:
    /// empty/empty is an inert no-op, cell-to-hand takes the cell, hand-to-cell
    /// places it, and occupied/occupied swaps both whole stacks. The bounds
    /// precheck runs before the cursor is touched, so an out-of-range index is
    /// [`CraftError::OutOfBounds`] with everything unchanged.
    pub fn click_grid_cell(
        &mut self,
        kind: GridKind,
        index: usize,
    ) -> Result<(), InventoryCommandError> {
        if index >= kind.slot_count() {
            return Err(CraftError::OutOfBounds.into());
        }

        let grid = match kind {
            GridKind::Player => &mut self.player_grid,
            GridKind::Table => &mut self.table_grid,
        };
        let cell = grid.slot(index);
        let cursor = self.inventory.cursor();

        match (cursor, cell) {
            (None, None) => Ok(()),
            (None, Some(_)) => {
                // Cell to hand. The index is in range, so `take` cannot fail.
                let item = grid
                    .take(index)
                    .map_err(InventoryCommandError::Craft)?
                    .expect("occupied cell taken");
                // The cursor was empty, so `put_cursor` cannot fail.
                self.inventory
                    .put_cursor(item)
                    .map_err(InventoryCommandError::Inventory)
            }
            (Some(_), None) => {
                // Hand to cell. Restore the cursor if the set ever rejected.
                let item = self.inventory.take_cursor()?;
                if let Err(error) = grid.set(index, Some(item)) {
                    let _ = self.inventory.put_cursor(item);
                    return Err(InventoryCommandError::Craft(error));
                }
                Ok(())
            }
            (Some(_), Some(_)) => {
                // Swap both whole stacks. After the bounds precheck no step can
                // fail, but every failure path restores the original cursor and
                // leaves the grid cell unchanged where possible.
                let hand = self.inventory.take_cursor()?;
                let cell_item = match grid.take(index) {
                    Ok(Some(item)) => item,
                    Ok(None) => {
                        let _ = self.inventory.put_cursor(hand);
                        return Ok(());
                    }
                    Err(error) => {
                        let _ = self.inventory.put_cursor(hand);
                        return Err(InventoryCommandError::Craft(error));
                    }
                };
                if let Err(error) = grid.set(index, Some(hand)) {
                    let _ = grid.set(index, Some(cell_item));
                    let _ = self.inventory.put_cursor(hand);
                    return Err(InventoryCommandError::Craft(error));
                }
                self.inventory
                    .put_cursor(cell_item)
                    .map_err(InventoryCommandError::Inventory)
            }
        }
    }

    /// Right-click grid-cell command: split-half or place-one.
    ///
    /// An empty cursor takes half (rounded up) of the cell stack onto the
    /// hand, leaving the remainder in the cell. An occupied cursor places
    /// exactly one unit into an empty or matching cell. A different identity
    /// is [`CraftError::CellOccupied`] and a full cell is
    /// [`CraftError::CellFull`]; both leave the grid and cursor unchanged.
    pub fn click_grid_cell_right(
        &mut self,
        kind: GridKind,
        index: usize,
    ) -> Result<(), InventoryCommandError> {
        if index >= kind.slot_count() {
            return Err(CraftError::OutOfBounds.into());
        }

        let grid = match kind {
            GridKind::Player => &mut self.player_grid,
            GridKind::Table => &mut self.table_grid,
        };
        let cell = grid.slot(index);

        match (self.inventory.cursor(), cell) {
            (None, None) => Ok(()),
            (None, Some(cell_item)) => {
                // Cell to hand, split half. The index is in range, so the set
                // cannot fail; the cursor is empty, so put_cursor cannot fail.
                let (moved, remainder) = cell_item.split_half();
                grid.set(index, remainder)?;
                self.inventory.put_cursor(moved)?;
                Ok(())
            }
            (Some(_), None) => {
                // Hand to empty cell, one unit. Take the hand first so a
                // rejected set can restore the exact pre-command cursor.
                let hand = self.inventory.take_cursor()?;
                let (moved, remainder) = hand.split_one();
                if let Err(error) = grid.set(index, Some(moved)) {
                    let _ = self.inventory.put_cursor(hand);
                    return Err(InventoryCommandError::Craft(error));
                }
                if let Some(remainder) = remainder {
                    self.inventory.put_cursor(remainder)?;
                }
                Ok(())
            }
            (Some(_), Some(cell_item)) => {
                // Hand to matching cell, one unit up to the cell's limit.
                let hand = self.inventory.take_cursor()?;
                if !hand.mergeable_with(&cell_item) {
                    let _ = self.inventory.put_cursor(hand);
                    return Err(CraftError::CellOccupied.into());
                }
                let limit = cell_item.id().stack_limit();
                if cell_item.count() >= limit {
                    let _ = self.inventory.put_cursor(hand);
                    return Err(CraftError::CellFull.into());
                }
                let (moved, remainder) = hand.split_one();
                let merged = cell_item
                    .with_count(cell_item.count() + moved.count())
                    .expect("right-click merge stays within the stack limit");
                if let Err(error) = grid.set(index, Some(merged)) {
                    let _ = self.inventory.put_cursor(hand);
                    return Err(InventoryCommandError::Craft(error));
                }
                if let Some(remainder) = remainder {
                    self.inventory.put_cursor(remainder)?;
                }
                Ok(())
            }
        }
    }

    /// Crafts the matching grid's recipe exactly once.
    ///
    /// Delegates to [`CraftingGrid::take_result`], so recipe revalidation and
    /// one-unit-per-cell consumption stay atomic in the domain: no recipe or a
    /// full destination is an error with nothing consumed.
    pub fn take_craft_result(&mut self, kind: GridKind) -> Result<Item, CraftError> {
        match kind {
            GridKind::Player => self.player_grid.take_result(&mut self.inventory),
            GridKind::Table => self.table_grid.take_result(&mut self.inventory),
        }
    }

    /// Read-only recipe preview for the matching grid. Never mutates.
    pub fn craft_preview(&self, kind: GridKind) -> Option<RecipePreview> {
        match kind {
            GridKind::Player => self.player_grid.preview(),
            GridKind::Table => self.table_grid.preview(),
        }
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

        // Submersion is derived from the already-in-scope `get_block` closure —
        // no signature change and no new call-site parameter. Sampling the feet
        // block catches a wading body, and the mid-body block keeps a body whose
        // feet have just cleared the surface buoyant until it is actually out of
        // the water (a generated ocean fills every column below `SEA_LEVEL`).
        let body_x = self.position.x.floor() as i32;
        let body_z = self.position.z.floor() as i32;
        let feet_block_y = (self.position.y + 1e-4).floor() as i32;
        let mid_block_y = (self.position.y + PLAYER_HEIGHT * 0.5).floor() as i32;
        let submerged = get_block(body_x, feet_block_y, body_z).is_liquid()
            || get_block(body_x, mid_block_y, body_z).is_liquid();

        let mut walk_speed = if input.sprint || input.crouch_or_down {
            7.2
        } else {
            4.6
        };
        if submerged {
            walk_speed *= SWIM_WALK_SPEED_MULT;
        }
        self.velocity.x = move_dir.x * walk_speed;
        self.velocity.z = move_dir.z * walk_speed;

        // Swim branch of walk mode. Honesty note: this asserts mechanics only
        // (positions, buoyancy, on_ground). No fall-damage or health claim is made
        // anywhere in this slice — no health/fall/damage system exists in `src/`;
        // fall damage and drowning are deferred to M008 (MEM227).
        if submerged {
            // Water drag on the vertical axis, then net upward buoyancy
            // (`SWIM_BUOYANCY_ACCEL` exceeds the 28.0 gravity below).
            let drag = (1.0 - SWIM_VERTICAL_DRAG * dt).clamp(0.0, 1.0);
            self.velocity.y = self.velocity.y * drag + (SWIM_BUOYANCY_ACCEL - 28.0) * dt;
            if input.jump_or_up {
                self.velocity.y = self.velocity.y.max(SWIM_RISE_SPEED);
            }
            self.velocity.y = self.velocity.y.clamp(-SWIM_MAX_SINK_SPEED, SWIM_RISE_SPEED);
            // A body in water is never standing: no on-ground land jump of 9.0.
            self.on_ground = false;
        } else {
            if self.on_ground && input.jump_or_up {
                self.velocity.y = 9.0;
                self.on_ground = false;
            }

            self.velocity.y = (self.velocity.y - 28.0 * dt).max(-45.0);
        }

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

    // -- Inventory-screen command façade -------------------------------------

    use crate::player::inventory::{ItemId, Material, RecipeId, INVENTORY_SLOTS, MAX_STACK_COUNT};

    fn block_item(block: BlockId, count: u16) -> Item {
        Item::new(ItemId::Block(block), count).expect("valid test block stack")
    }

    fn stick_item(count: u16) -> Item {
        Item::new(ItemId::Material(Material::Stick), count).expect("valid test sticks")
    }

    fn grid_units(grid: &CraftingGrid) -> u32 {
        (0..grid.kind().slot_count())
            .filter_map(|index| grid.slot(index))
            .map(|item| u32::from(item.count()))
            .sum()
    }

    /// Total item units across the 36 slots, the cursor, and both grids.
    /// Any command that changes this number has duplicated or destroyed items.
    fn player_units(player: &Player) -> u32 {
        let inventory = player.inventory();
        let slots = inventory
            .slots()
            .iter()
            .flatten()
            .map(|item| u32::from(item.count()))
            .sum::<u32>();
        let cursor = inventory.cursor().map_or(0, |item| u32::from(item.count()));
        slots + cursor + grid_units(player.player_grid()) + grid_units(player.table_grid())
    }

    fn player_with_slots<const N: usize>(slots: [(usize, Item); N]) -> Player {
        let mut all = [None; INVENTORY_SLOTS];
        for (index, item) in slots {
            all[index] = Some(item);
        }
        Player::with_inventory(
            Vec3::ZERO,
            Inventory::from_parts(all, None),
            CraftingGrid::new(GridKind::Player),
            CraftingGrid::new(GridKind::Table),
        )
    }

    #[test]
    fn slot_click_picks_up_then_places_the_whole_stack() {
        let mut player =
            player_with_slots([(3, stick_item(5)), (7, block_item(BlockId::Stone, 2))]);
        let units = player_units(&player);

        player
            .click_inventory_slot(3)
            .expect("empty cursor picks up");
        assert_eq!(player.inventory().cursor(), Some(stick_item(5)));
        assert_eq!(player.inventory().slot(3), None);
        assert_eq!(player_units(&player), units, "pick-up conserves items");

        player
            .click_inventory_slot(7)
            .expect("occupied cursor places a different identity by swapping");
        assert_eq!(player.inventory().slot(7), Some(stick_item(5)));
        assert_eq!(
            player.inventory().cursor(),
            Some(block_item(BlockId::Stone, 2))
        );
        assert_eq!(player.inventory().slot(3), None);
        assert_eq!(player_units(&player), units, "swap conserves items");

        // Placing on an empty slot empties the cursor.
        player
            .click_inventory_slot(3)
            .expect("place into empty slot");
        assert_eq!(
            player.inventory().slot(3),
            Some(block_item(BlockId::Stone, 2))
        );
        assert_eq!(player.inventory().cursor(), None);
        assert_eq!(player_units(&player), units);
    }

    #[test]
    fn slot_click_swaps_different_identities() {
        let mut player =
            player_with_slots([(0, block_item(BlockId::Stone, 3)), (1, stick_item(2))]);
        let units = player_units(&player);

        player.click_inventory_slot(0).expect("pick up stone");
        assert_eq!(
            player.inventory().cursor(),
            Some(block_item(BlockId::Stone, 3))
        );

        player
            .click_inventory_slot(1)
            .expect("swap stone for sticks");
        assert_eq!(
            player.inventory().slot(1),
            Some(block_item(BlockId::Stone, 3))
        );
        assert_eq!(player.inventory().cursor(), Some(stick_item(2)));
        assert_eq!(player.inventory().slot(0), None);
        assert_eq!(player_units(&player), units);
    }

    #[test]
    fn slot_click_out_of_bounds_preserves_inventory_and_cursor() {
        let mut player =
            player_with_slots([(0, block_item(BlockId::Stone, 4)), (1, stick_item(1))]);
        let before = player.inventory().clone();

        assert_eq!(
            player.click_inventory_slot(INVENTORY_SLOTS),
            Err(InventoryError::OutOfBounds)
        );
        assert_eq!(
            player.click_inventory_slot(usize::MAX),
            Err(InventoryError::OutOfBounds)
        );
        assert_eq!(player.inventory(), &before);

        player.click_inventory_slot(0).expect("pick up stone");
        let after_pick_up = player.inventory().clone();
        assert_eq!(
            player.click_inventory_slot(INVENTORY_SLOTS),
            Err(InventoryError::OutOfBounds)
        );
        assert_eq!(
            player.inventory(),
            &after_pick_up,
            "an out-of-range put-down keeps the cursor stack"
        );
    }

    #[test]
    fn grid_click_takes_places_and_swaps_while_conserving_items() {
        let mut player =
            player_with_slots([(0, block_item(BlockId::Stone, 4)), (1, stick_item(1))]);
        let mut player_grid = CraftingGrid::new(GridKind::Player);
        player_grid
            .set(0, Some(block_item(BlockId::OakPlanks, 5)))
            .expect("cell 0");
        player.player_grid = player_grid;

        let units = player_units(&player);

        // 1. Cell to hand.
        player
            .click_grid_cell(GridKind::Player, 0)
            .expect("cell to hand");
        assert_eq!(
            player.inventory().cursor(),
            Some(block_item(BlockId::OakPlanks, 5))
        );
        assert_eq!(player.player_grid().slot(0), None);
        assert_eq!(player_units(&player), units, "cell -> hand conserves");

        // 2. Hand to empty cell.
        player
            .click_grid_cell(GridKind::Player, 1)
            .expect("hand to cell");
        assert_eq!(player.inventory().cursor(), None);
        assert_eq!(
            player.player_grid().slot(1),
            Some(block_item(BlockId::OakPlanks, 5))
        );
        assert_eq!(player.player_grid().slot(0), None);
        assert_eq!(player_units(&player), units, "hand -> cell conserves");

        // 3. Whole-stack swap with an occupied cell.
        player.click_inventory_slot(0).expect("pick up stone");
        assert_eq!(player_units(&player), units);
        player
            .click_grid_cell(GridKind::Player, 1)
            .expect("swap hand with cell");
        assert_eq!(
            player.inventory().cursor(),
            Some(block_item(BlockId::OakPlanks, 5))
        );
        assert_eq!(
            player.player_grid().slot(1),
            Some(block_item(BlockId::Stone, 4))
        );
        assert_eq!(player_units(&player), units, "grid swap conserves");

        // Empty cursor + empty cell is an inert no-op.
        player
            .click_inventory_slot(0)
            .expect("empty slot zero receives the planks back");
        assert_eq!(player.inventory().cursor(), None);
        player
            .click_grid_cell(GridKind::Player, 3)
            .expect("empty/empty is a no-op");
        assert_eq!(player.player_grid().slot(3), None);
        assert_eq!(player_units(&player), units);
    }

    #[test]
    fn right_click_slot_splits_a_stack_and_places_single_units() {
        let mut player = player_with_slots([
            (0, block_item(BlockId::OakPlanks, 5)),
            (1, block_item(BlockId::OakPlanks, 2)),
            (2, block_item(BlockId::Stone, 1)),
        ]);
        let units = player_units(&player);

        // Empty cursor + right click: take half (rounded up).
        player
            .click_inventory_slot_right(0)
            .expect("split half of five planks");
        assert_eq!(
            player.inventory().cursor().map(|item| item.count()),
            Some(3)
        );
        assert_eq!(
            player.inventory().hotbar(0).map(|item| item.count()),
            Some(2)
        );
        assert_eq!(player_units(&player), units);

        // Occupied cursor + right click on a matching stack: place exactly one.
        player
            .click_inventory_slot_right(1)
            .expect("place one plank into the matching stack");
        assert_eq!(
            player.inventory().hotbar(1).map(|item| item.count()),
            Some(3)
        );
        assert_eq!(
            player.inventory().cursor().map(|item| item.count()),
            Some(2)
        );
        assert_eq!(player_units(&player), units);

        // Occupied cursor + right click on an empty slot: place exactly one.
        player
            .click_inventory_slot_right(3)
            .expect("place one plank into an empty slot");
        assert_eq!(
            player.inventory().hotbar(3).map(|item| item.count()),
            Some(1)
        );
        assert_eq!(
            player.inventory().cursor().map(|item| item.count()),
            Some(1)
        );
        assert_eq!(player_units(&player), units);

        // Different identity is rejected without a swap.
        let before = player.inventory().clone();
        assert_eq!(
            player.click_inventory_slot_right(2),
            Err(InventoryError::SlotOccupied)
        );
        assert_eq!(player.inventory(), &before);
    }

    #[test]
    fn right_click_grid_places_one_into_a_cell_and_splits_from_a_cell() {
        let mut player = player_with_slots([(0, block_item(BlockId::OakPlanks, 7))]);
        let units = player_units(&player);

        // Pick up half the stack onto the hand, then right-click a grid cell:
        // exactly one plank enters the cell so a shaped recipe can be built.
        player
            .click_inventory_slot_right(0)
            .expect("hand holds four planks");
        assert_eq!(
            player.inventory().cursor().map(|item| item.count()),
            Some(4)
        );

        player
            .click_grid_cell_right(GridKind::Player, 0)
            .expect("place one into the grid");
        assert_eq!(
            player.player_grid().slot(0).map(|item| item.count()),
            Some(1)
        );
        assert_eq!(
            player.inventory().cursor().map(|item| item.count()),
            Some(3)
        );

        // Right-clicking the same cell again tops it up by one.
        player
            .click_grid_cell_right(GridKind::Player, 0)
            .expect("top the cell up by one");
        assert_eq!(
            player.player_grid().slot(0).map(|item| item.count()),
            Some(2)
        );
        assert_eq!(
            player.inventory().cursor().map(|item| item.count()),
            Some(2)
        );
        assert_eq!(player_units(&player), units);

        // Empty hand + right click a cell: take half, leaving the remainder.
        player
            .click_grid_cell(GridKind::Player, 0)
            .expect("a left click still takes the whole cell");
        assert_eq!(
            player.inventory().cursor().map(|item| item.count()),
            Some(2)
        );
        player
            .click_grid_cell_right(GridKind::Player, 1)
            .expect("place one in the second cell");
        assert_eq!(
            player.player_grid().slot(1).map(|item| item.count()),
            Some(1)
        );
        assert_eq!(
            player.inventory().cursor().map(|item| item.count()),
            Some(1)
        );

        player.click_inventory_slot(4).expect("park the last plank");
        player
            .click_grid_cell_right(GridKind::Player, 1)
            .expect("empty hand splits the cell");
        assert_eq!(
            player.inventory().cursor().map(|item| item.count()),
            Some(1)
        );
        assert_eq!(player.player_grid().slot(1), None);
        assert_eq!(player_units(&player), units);
    }

    #[test]
    fn right_click_grid_rejects_different_or_full_cells_without_mutation() {
        let mut player = player_with_slots([(0, block_item(BlockId::OakPlanks, 2))]);
        let mut player_grid = CraftingGrid::new(GridKind::Player);
        player_grid
            .set(0, Some(block_item(BlockId::Stone, 1)))
            .expect("stone cell");
        player_grid
            .set(1, Some(block_item(BlockId::OakPlanks, MAX_STACK_COUNT)))
            .expect("full plank cell");
        player.player_grid = player_grid;
        player
            .click_inventory_slot(0)
            .expect("hand holds two planks");

        let before_inventory = player.inventory().clone();
        let before_grid = player.player_grid().clone();
        for (index, error) in [
            (0, CraftError::CellOccupied),
            (1, CraftError::CellFull),
            (4, CraftError::OutOfBounds),
        ] {
            assert_eq!(
                player.click_grid_cell_right(GridKind::Player, index),
                Err(InventoryCommandError::Craft(error))
            );
            assert_eq!(player.inventory(), &before_inventory);
            assert_eq!(player.player_grid(), &before_grid);
        }
    }

    #[test]
    fn grid_click_out_of_bounds_preserves_cursor_and_grid() {
        let mut player =
            player_with_slots([(0, block_item(BlockId::Stone, 2)), (1, stick_item(1))]);
        let mut table_grid = CraftingGrid::new(GridKind::Table);
        table_grid
            .set(15, Some(block_item(BlockId::IronOre, 3)))
            .expect("cell 15");
        player.table_grid = table_grid;
        player.click_inventory_slot(0).expect("cursor holds stone");

        let before = player.inventory().clone();
        let player_grid_before = player.player_grid().clone();
        let table_grid_before = player.table_grid().clone();

        assert_eq!(
            player.click_grid_cell(GridKind::Player, 4),
            Err(InventoryCommandError::Craft(CraftError::OutOfBounds)),
            "the 2x2 grid has no index 4"
        );
        assert_eq!(
            player.click_grid_cell(GridKind::Table, 16),
            Err(InventoryCommandError::Craft(CraftError::OutOfBounds)),
            "the 4x4 grid has no index 16"
        );
        assert_eq!(
            player.click_grid_cell(GridKind::Player, usize::MAX),
            Err(InventoryCommandError::Craft(CraftError::OutOfBounds))
        );

        assert_eq!(player.inventory(), &before);
        assert_eq!(player.player_grid(), &player_grid_before);
        assert_eq!(player.table_grid(), &table_grid_before);
    }

    #[test]
    fn grid_click_cannot_move_items_between_the_two_grids() {
        let mut player = Player::new(Vec3::ZERO);
        let mut player_grid = CraftingGrid::new(GridKind::Player);
        player_grid
            .set(3, Some(block_item(BlockId::OakPlanks, 2)))
            .expect("player cell 3");
        let mut table_grid = CraftingGrid::new(GridKind::Table);
        table_grid
            .set(3, Some(stick_item(1)))
            .expect("table cell 3");
        player.player_grid = player_grid;
        player.table_grid = table_grid;

        // Index 3 exists in both grids; `kind` alone decides which one moves.
        player
            .click_grid_cell(GridKind::Player, 3)
            .expect("player cell to hand");
        assert_eq!(
            player.inventory().cursor(),
            Some(block_item(BlockId::OakPlanks, 2))
        );
        assert_eq!(player.player_grid().slot(3), None);
        assert_eq!(
            player.table_grid().slot(3),
            Some(stick_item(1)),
            "a player-grid command never reads the table grid"
        );

        player
            .click_grid_cell(GridKind::Table, 3)
            .expect("table cell swaps with hand");
        assert_eq!(player.inventory().cursor(), Some(stick_item(1)));
        assert_eq!(
            player.table_grid().slot(3),
            Some(block_item(BlockId::OakPlanks, 2))
        );
        assert_eq!(
            player.player_grid().slot(3),
            None,
            "a table-grid command never writes the player grid"
        );
    }

    #[test]
    fn take_craft_result_consumes_inputs_exactly_once() {
        let mut player = Player::new(Vec3::ZERO);
        let mut player_grid = CraftingGrid::new(GridKind::Player);
        for cell in 0..4 {
            player_grid
                .set(cell, Some(block_item(BlockId::OakPlanks, 1)))
                .expect("plank cell");
        }
        player.player_grid = player_grid;
        assert_eq!(
            player.craft_preview(GridKind::Player).map(|p| p.recipe),
            Some(RecipeId::CraftingTable)
        );

        let crafted = player
            .take_craft_result(GridKind::Player)
            .expect("2x2 planks craft one table");
        assert_eq!(crafted, block_item(BlockId::CraftingTable, 1));
        assert_eq!(player.player_grid().slot(0), None);
        assert_eq!(player.player_grid().slot(1), None);
        assert_eq!(player.player_grid().slot(2), None);
        assert_eq!(player.player_grid().slot(3), None);
        assert!(player.player_grid().is_empty());
        assert_eq!(
            player
                .inventory()
                .slots()
                .iter()
                .flatten()
                .filter(|item| item.id() == ItemId::Block(BlockId::CraftingTable))
                .count(),
            1,
            "exactly one output item is produced"
        );

        let after = player.inventory().clone();
        let grid_after = player.player_grid().clone();
        assert_eq!(
            player.take_craft_result(GridKind::Player),
            Err(CraftError::NoRecipe),
            "a second click cannot mint a second table"
        );
        assert_eq!(player.inventory(), &after);
        assert_eq!(player.player_grid(), &grid_after);
    }

    #[test]
    fn take_craft_result_without_a_recipe_or_full_destination_preserves_everything() {
        let mut player = Player::new(Vec3::ZERO);
        let empty = player.inventory().clone();
        assert_eq!(
            player.take_craft_result(GridKind::Player),
            Err(CraftError::NoRecipe)
        );
        assert_eq!(
            player.take_craft_result(GridKind::Table),
            Err(CraftError::NoRecipe)
        );
        assert_eq!(player.inventory(), &empty);

        let mut player_grid = CraftingGrid::new(GridKind::Player);
        player_grid
            .set(0, Some(block_item(BlockId::OakLog, 1)))
            .expect("log cell");
        player.player_grid = player_grid;

        // A completely full inventory cannot receive the output.
        let mut slots = [None; INVENTORY_SLOTS];
        for slot in slots.iter_mut() {
            *slot = Some(block_item(BlockId::Brick, 1));
        }
        player.inventory = Inventory::from_parts(slots, Some(stick_item(1)));
        let full_inventory = player.inventory().clone();
        let grid_before = player.player_grid().clone();

        assert_eq!(
            player.take_craft_result(GridKind::Player),
            Err(CraftError::DestinationFull)
        );
        assert_eq!(player.inventory(), &full_inventory);
        assert_eq!(
            player.player_grid(),
            &grid_before,
            "a rejected craft consumes no inputs"
        );
    }

    #[test]
    fn craft_preview_reads_only_and_matches_the_grid() {
        let mut player = Player::new(Vec3::ZERO);
        assert_eq!(player.craft_preview(GridKind::Player), None);
        assert_eq!(player.craft_preview(GridKind::Table), None);

        let mut player_grid = CraftingGrid::new(GridKind::Player);
        player_grid
            .set(0, Some(block_item(BlockId::OakLog, 1)))
            .expect("log cell");
        player.player_grid = player_grid;
        let mut table_grid = CraftingGrid::new(GridKind::Table);
        table_grid
            .set(0, Some(block_item(BlockId::OakPlanks, 2)))
            .expect("planks column");
        table_grid
            .set(4, Some(block_item(BlockId::OakPlanks, 2)))
            .expect("planks column");
        player.table_grid = table_grid;

        let before_inventory = player.inventory().clone();
        let before_player_grid = player.player_grid().clone();
        let before_table_grid = player.table_grid().clone();

        let preview = player
            .craft_preview(GridKind::Player)
            .expect("one log previews planks");
        assert_eq!(preview.recipe, RecipeId::Planks);
        assert_eq!(preview.output, ItemId::Block(BlockId::OakPlanks));
        assert_eq!(preview.output_count, 4);

        let table_preview = player
            .craft_preview(GridKind::Table)
            .expect("a 2x1 plank column previews sticks");
        assert_eq!(table_preview.recipe, RecipeId::Sticks);
        assert_eq!(table_preview.output, ItemId::Material(Material::Stick));
        assert_eq!(table_preview.output_count, 4);

        assert_eq!(player.inventory(), &before_inventory);
        assert_eq!(player.player_grid(), &before_player_grid);
        assert_eq!(player.table_grid(), &before_table_grid);
    }

    /// Sea level used by the pure water-column fixtures below. Mirrors
    /// `crate::world::terrain::SEA_LEVEL`; every column with `y < TEST_SEA_LEVEL`
    /// is Water and everything above it is Air, so the top water surface sits at
    /// height `TEST_SEA_LEVEL` exactly like a generated ocean.
    const TEST_SEA_LEVEL: i32 = crate::world::terrain::SEA_LEVEL;

    /// Solid seabed (`y < 0`), a 22-block water column (`0..TEST_SEA_LEVEL`), and
    /// open air above the surface. Water is never solid here, so the column is
    /// passable exactly as the production world is after S01.
    fn water_column(_x: i32, y: i32, _z: i32) -> BlockId {
        if y < 0 {
            BlockId::Stone
        } else if y < TEST_SEA_LEVEL {
            BlockId::Water
        } else {
            BlockId::Air
        }
    }

    /// Dry-land control column: Stone below y=5, Air above. Used only as the
    /// land-walk / land-jump reference the swim mechanics must differ from.
    fn dry_land_column(_x: i32, y: i32, _z: i32) -> BlockId {
        if y < 5 {
            BlockId::Stone
        } else {
            BlockId::Air
        }
    }

    #[test]
    fn swim_submerged_player_floats_up_to_the_surface_band() {
        // Released deep under the surface (3 blocks above the seabed top) with no
        // input at all: buoyancy must carry the body up to the sea surface and
        // hold it there instead of letting gravity walk the seabed.
        let mut player = Player::new(Vec3::new(0.5, 3.0, 0.5));
        player.fly_mode = false;
        let input = InputState::default();
        let mut lowest = player.position.y;
        for _ in 0..600 {
            player.update(1.0 / 60.0, &input, water_column);
            lowest = lowest.min(player.position.y);
        }
        assert!(
            lowest > 1.0,
            "a submerged player must never sink toward the seabed (lowest y {lowest})"
        );
        assert!(
            (player.position.y - TEST_SEA_LEVEL as f32).abs() < 0.5,
            "expected the body to settle in the surface band around y={TEST_SEA_LEVEL}, got {}",
            player.position.y
        );
        assert!(
            !player.on_ground,
            "a floating player is not standing on ground"
        );
    }

    #[test]
    fn swim_holding_jump_or_up_rises_out_of_the_water() {
        let mut player = Player::new(Vec3::new(0.5, 3.0, 0.5));
        player.fly_mode = false;
        let rise_input = InputState {
            jump_or_up: true,
            ..Default::default()
        };
        let mut exited = false;
        for _ in 0..600 {
            player.update(1.0 / 60.0, &rise_input, water_column);
            if player.position.y >= TEST_SEA_LEVEL as f32 {
                exited = true;
                break;
            }
        }
        assert!(
            exited,
            "holding Space must rise through the column and clear the surface, y={}",
            player.position.y
        );
    }

    #[test]
    fn swim_vertical_drag_bounds_the_float_speed() {
        let mut player = Player::new(Vec3::new(0.5, 2.0, 0.5));
        player.fly_mode = false;
        let input = InputState::default();
        let mut max_step = 0.0f32;
        let mut settled_min = f32::INFINITY;
        let mut settled_max = f32::NEG_INFINITY;
        for frame in 0..1200 {
            let before = player.position.y;
            player.update(1.0 / 60.0, &input, water_column);
            let dy = (player.position.y - before).abs();
            if before < (TEST_SEA_LEVEL - 2) as f32 {
                max_step = max_step.max(dy);
            }
            if frame >= 900 {
                settled_min = settled_min.min(player.position.y);
                settled_max = settled_max.max(player.position.y);
            }
        }
        assert!(
            max_step > 0.0,
            "buoyancy must actually move the body (max per-frame step {max_step})"
        );
        assert!(
            max_step < 0.1,
            "vertical drag must bound the float step, got {max_step} blocks/frame (teleport-like)"
        );
        assert!(
            settled_max - settled_min < 0.4,
            "the float must be gentle, not oscillating: band {}..{settled_min}..{settled_max}",
            settled_max
        );
    }

    #[test]
    fn swim_horizontal_speed_is_slower_than_land_walk_and_water_is_passable() {
        let forward = InputState {
            forward: true,
            ..Default::default()
        };

        let mut walker = Player::new(Vec3::new(0.5, 5.0, 0.5));
        walker.fly_mode = false;
        walker.yaw = 0.0; // forward => +x
        for _ in 0..60 {
            walker.update(1.0 / 60.0, &forward, dry_land_column);
        }
        let land_dx = walker.position.x - 0.5;
        assert!(
            land_dx > 4.0,
            "land walk covers ~4.6 blocks in 1s, got {land_dx}"
        );

        let mut swimmer = Player::new(Vec3::new(0.5, (TEST_SEA_LEVEL - 5) as f32, 0.5));
        swimmer.fly_mode = false;
        swimmer.yaw = 0.0;
        let start_x = swimmer.position.x;
        for _ in 0..60 {
            swimmer.update(1.0 / 60.0, &forward, water_column);
        }
        let swim_dx = swimmer.position.x - start_x;
        assert!(
            swim_dx > 0.0,
            "water is passable: the player must move freely through it, got {swim_dx}"
        );
        assert!(
            swim_dx < land_dx * 0.9,
            "swimming must be slower than land walking: {swim_dx} vs {land_dx}"
        );
    }

    #[test]
    fn swim_does_not_change_fly_mode_behavior() {
        // The fly branch returns before any swim code: a fly-mode player inside a
        // water column with no input keeps its velocity (no buoyancy, no drag).
        let mut flyer = Player::new(Vec3::new(0.5, 5.0, 0.5));
        flyer.fly_mode = true;
        let input = InputState::default();
        for _ in 0..30 {
            flyer.update(1.0 / 60.0, &input, water_column);
        }
        assert_eq!(flyer.position.y, 5.0, "fly mode ignores water buoyancy");
        assert_eq!(flyer.velocity, Vec3::ZERO);

        // ...and above water it still rises at the fly speed while Space is held.
        let mut flyer_above = Player::new(Vec3::new(0.5, 100.0, 0.5));
        flyer_above.fly_mode = true;
        let up = InputState {
            jump_or_up: true,
            ..Default::default()
        };
        flyer_above.update(0.1, &up, water_column);
        assert!(flyer_above.position.y > 100.0);
        assert!(
            (flyer_above.velocity.y - 13.5).abs() < 1e-4,
            "fly rise speed must stay untouched (18.0 * 0.75), got {}",
            flyer_above.velocity.y
        );
    }

    #[test]
    fn swim_water_is_never_walkable_on_its_surface() {
        // Dropped exactly at the surface: the player sinks into the water and is
        // pushed back up, but can never stand on the liquid (no solid block, so
        // `on_ground` never becomes true and the water surface is not walkable).
        assert!(
            !BlockId::Water.is_solid(),
            "water must stay passable-not-walkable"
        );
        let mut player = Player::new(Vec3::new(0.5, TEST_SEA_LEVEL as f32, 0.5));
        player.fly_mode = false;
        let input = InputState::default();
        for _ in 0..600 {
            player.update(1.0 / 60.0, &input, water_column);
            assert!(
                !player.on_ground,
                "a player must never stand on the water surface (y={})",
                player.position.y
            );
        }
    }

    #[test]
    fn swim_does_not_replace_the_land_jump_velocity() {
        // Dry land: the on-ground jump of 9.0 is unchanged.
        let mut land = Player::new(Vec3::new(0.5, 5.0, 0.5));
        land.fly_mode = false;
        land.on_ground = true;
        let jump = InputState {
            jump_or_up: true,
            ..Default::default()
        };
        land.update(1.0 / 60.0, &jump, dry_land_column);
        assert!(
            land.velocity.y > 8.0,
            "land jump must still launch at ~9.0, got {}",
            land.velocity.y
        );

        // Submerged: Space drives a bounded swim rise instead of the 9.0 jump.
        let mut swimmer = Player::new(Vec3::new(0.5, 3.0, 0.5));
        swimmer.fly_mode = false;
        swimmer.update(1.0 / 60.0, &jump, water_column);
        assert!(
            swimmer.velocity.y <= SWIM_RISE_SPEED + 1e-3,
            "swim rise must not reuse the 9.0 land jump, got {}",
            swimmer.velocity.y
        );
        assert!(swimmer.velocity.y > 0.0);
    }

    /// Physics site of the S03 water mobility matrix: water is passable (no
    /// collision stop) but not walkable (no standing on the surface, no walking
    /// the seabed).
    ///
    /// The four `is_solid` consumer sites and their named proof:
    /// 1. Physics collision (`collides_with_world` in this module) — asserted here.
    /// 2. Voxel raycast (`src/player/raycast.rs`) —
    ///    `raycast_skips_water_and_hits_the_stone_seabed`.
    /// 3. Greedy mesh (`src/mesh/greedy.rs`, `water_quads`) — already pinned by
    ///    `water_only_padded_chunk_emits_water_quads_and_no_opaque_quads` and
    ///    `water_does_not_cull_stone_faces`, cited rather than duplicated.
    /// 4. Sky light (`src/world/light.rs`) —
    ///    `water_column_is_sky_lit_like_air_and_never_occludes_like_a_solid_roof`.
    ///
    /// The `BlockId` solid/liquid matrix itself is pinned by
    /// `test_solid_and_liquid_matrix_over_every_block_id` in `src/world/block.rs`.
    ///
    /// Honesty note: no fall-damage or health claim is made anywhere in S03. No
    /// health/fall/damage system exists in `src/` (grep-verified during
    /// research); fall damage and drowning are deferred to M008 (MEM227). Swim
    /// assertions cover mechanics only (positions, buoyancy, `on_ground`).
    #[test]
    fn water_is_passable_but_never_walkable_at_the_physics_consumer() {
        assert!(!BlockId::Water.is_solid(), "water must stay passable");
        assert!(
            BlockId::Water.is_liquid(),
            "water is the liquid block this matrix is about"
        );

        // Passable: a walk-mode player moves horizontally through the water
        // column at the reduced swim speed with no collision stop. `update`
        // resolves X/Z against `collides_with_world`, which only sees solid
        // blocks, so the water cells cannot block the body.
        let mut swimmer = Player::new(Vec3::new(0.5, 10.0, 0.5));
        swimmer.fly_mode = false;
        swimmer.yaw = 0.0;
        let forward = InputState {
            forward: true,
            ..Default::default()
        };
        let start_x = swimmer.position.x;
        for _ in 0..60 {
            swimmer.update(1.0 / 60.0, &forward, water_column);
        }
        assert!(
            swimmer.position.x > start_x + 2.0,
            "water must not stop horizontal movement: x {} -> {}",
            start_x,
            swimmer.position.x
        );
        assert!(
            swimmer.position.x < start_x + 4.6,
            "swimming must stay slower than the 4.6 m/s land walk: x {} -> {}",
            start_x,
            swimmer.position.x
        );

        // Not walkable: released exactly at the surface with no input, buoyancy
        // keeps the body floating in the surface band; it never stands on the
        // liquid and never settles onto the seabed (the pre-S03 failure: gravity
        // walked the body down through the passable water and onto the seabed).
        let mut floater = Player::new(Vec3::new(0.5, TEST_SEA_LEVEL as f32, 0.5));
        floater.fly_mode = false;
        let idle = InputState::default();
        let mut lowest = floater.position.y;
        for _ in 0..600 {
            floater.update(1.0 / 60.0, &idle, water_column);
            lowest = lowest.min(floater.position.y);
            assert!(
                !floater.on_ground,
                "water is not walkable: on_ground became true at y={}",
                floater.position.y
            );
        }
        assert!(
            lowest > 1.0,
            "buoyancy must keep the body off the seabed (lowest y {lowest})"
        );
        assert!(
            (floater.position.y - TEST_SEA_LEVEL as f32).abs() < 0.5,
            "the body must float in the surface band around y={TEST_SEA_LEVEL}, got {}",
            floater.position.y
        );
    }
}
