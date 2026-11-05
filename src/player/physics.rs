use glam::Vec3;

use crate::content::ContentRegistry;

use crate::player::combat::PlayerCombat;
use crate::player::container::Container;
use crate::player::eating::{
    food_value, food_value_with, food_value_with_registry_rules, EatingRules,
};
use crate::player::inventory::{
    CollectBatchOutcome, CollectOutcome, CraftError, CraftingGrid, GridKind, Inventory,
    InventoryError, Item, ItemError, ItemId, ItemStack, RecipePreview, ToolWear, HOTBAR_SLOTS,
};
use crate::player::survival::{GameMode, SurvivalState, MAX_HUNGER};
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

/// Per-second exponential drag on vertical velocity while submerged (D087).
/// With gravity this bounds an idle sink at roughly 4.7 m/s: deliberate, never
/// a teleport.
pub const SWIM_VERTICAL_DRAG: f32 = 6.0;
/// Upward acceleration while `jump_or_up` is held underwater (D087). Net of the
/// 28.0 m/s^2 gravity below this lifts a submerged body; released, gravity plus
/// drag make it sink. There is no buoyant surface lock in either case.
pub const SWIM_RISE_ACCEL: f32 = 40.0;
/// Upward speed while `jump_or_up` is held underwater. Deliberately below the
/// 9.0 m/s land jump so swimming up reads as a steady rise, not a leap.
pub const SWIM_RISE_SPEED: f32 = 4.0;
/// Terminal sink speed underwater, so entering from a long fall never slams the
/// body into the seabed at the 45 m/s dry-land terminal velocity.
pub const SWIM_MAX_SINK_SPEED: f32 = 6.0;
/// Horizontal walk-speed multiplier while submerged: 4.6 * 0.55 = 2.53 m/s, so
/// swimming is always slower than the 4.6 m/s land walk (and the 7.2 sprint).
pub const SWIM_WALK_SPEED_MULT: f32 = 0.55;
/// Downward acceleration applied while submerged, in blocks per second
/// squared. The swim branch subtracts this so the idle sink keeps matching the
/// dry-land gravity feel while drag bounds the terminal speed.
pub const SWIM_SINK_ACCEL: f32 = 28.0;

/// Horizontal walking speed on land, in blocks per second.
pub const WALK_SPEED: f32 = 4.6;
/// Horizontal sprint speed in blocks per second. Crouching deliberately uses
/// the sprint speed too, matching the pre-config behavior.
pub const SPRINT_SPEED: f32 = 7.2;
/// Flight speed in blocks per second while not sprinting.
pub const FLY_SPEED: f32 = 18.0;
/// Flight speed in blocks per second while sprinting.
pub const FLY_SPRINT_SPEED: f32 = 48.0;
/// Fraction of flight speed applied to vertical movement.
pub const FLY_VERTICAL_FACTOR: f32 = 0.75;
/// Upward launch speed of a standing jump, in blocks per second.
pub const JUMP_VELOCITY: f32 = 9.0;
/// Downward gravity acceleration, in blocks per second squared.
pub const GRAVITY: f32 = 28.0;
/// Maximum downward fall speed, in blocks per second.
pub const TERMINAL_VELOCITY: f32 = 45.0;

/// Effective movement tuning for one player, in blocks and seconds.
///
/// The defaults reference the named constants above, so a fresh player is
/// behavior-identical to the pre-config rules. Startup and live reload replace
/// this wholesale through [`crate::world::session::WorldSession::apply_rules`];
/// `Player::update` never reads the bare constants, so an applied rule set is
/// the only thing that steers movement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MovementRules {
    pub walk_speed: f32,
    pub sprint_speed: f32,
    pub fly_speed: f32,
    pub fly_sprint_speed: f32,
    pub fly_vertical_factor: f32,
    pub jump_velocity: f32,
    pub gravity: f32,
    pub terminal_velocity: f32,
    pub swim_vertical_drag: f32,
    pub swim_rise_accel: f32,
    pub swim_rise_speed: f32,
    pub swim_max_sink_speed: f32,
    pub swim_walk_speed_mult: f32,
    pub swim_sink_accel: f32,
}

impl Default for MovementRules {
    fn default() -> Self {
        Self {
            walk_speed: WALK_SPEED,
            sprint_speed: SPRINT_SPEED,
            fly_speed: FLY_SPEED,
            fly_sprint_speed: FLY_SPRINT_SPEED,
            fly_vertical_factor: FLY_VERTICAL_FACTOR,
            jump_velocity: JUMP_VELOCITY,
            gravity: GRAVITY,
            terminal_velocity: TERMINAL_VELOCITY,
            swim_vertical_drag: SWIM_VERTICAL_DRAG,
            swim_rise_accel: SWIM_RISE_ACCEL,
            swim_rise_speed: SWIM_RISE_SPEED,
            swim_max_sink_speed: SWIM_MAX_SINK_SPEED,
            swim_walk_speed_mult: SWIM_WALK_SPEED_MULT,
            swim_sink_accel: SWIM_SINK_ACCEL,
        }
    }
}

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

    /// Whether any movement key is held this frame.
    ///
    /// Hold-to-eat cancels on movement (see [`crate::player::eating`]), so the
    /// check lives next to the flags themselves rather than being re-derived
    /// from them at the call site.
    pub fn is_moving(&self) -> bool {
        self.forward
            || self.backward
            || self.left
            || self.right
            || self.jump_or_up
            || self.crouch_or_down
            || self.sprint
    }
}

/// Dry-land fall measurement carried by the player's physics step.
///
/// Physics measures distance only; damage, hunger, and air rules live in
/// [`crate::player::survival`] and are applied by `WorldSession::tick_survival`
/// (S04). Water contact always cancels a pending fall and consumes the armed
/// spawn immunity, because touching water counts as the first contact.
#[derive(Clone, Debug, Default)]
struct FallState {
    /// Highest feet height seen since the body last left the ground.
    peak_y: Option<f32>,
    /// One-shot dry-land distance latched on landing, consumed by the session.
    landing_distance: f32,
    /// Discards the first ground (or water) contact after a fresh spawn or
    /// respawn.
    spawn_immune: bool,
}

impl FallState {
    /// Drops all fall bookkeeping, including spawn immunity.
    fn clear(&mut self) {
        *self = Self::default();
    }

    /// Drops the in-flight fall and any latched landing but keeps spawn
    /// immunity.
    fn clear_pending(&mut self) {
        self.peak_y = None;
        self.landing_distance = 0.0;
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
    /// Effective movement tuning. Starts at [`MovementRules::default`] and is
    /// replaced only through the config seam
    /// ([`crate::world::session::WorldSession::apply_rules`]).
    pub movement: MovementRules,
    /// Persisted vitals. Constructed only through checked `SurvivalState`
    /// constructors, so an out-of-range save record never reaches this field.
    pub survival: SurvivalState,
    /// Effective mode. Private with a mode checked by `GameMode::allows_fly`;
    /// `fly_mode` alone can never grant Survival flight.
    mode: GameMode,
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
    /// Dry-land fall measurement and one-shot spawn immunity (S04).
    fall: FallState,
    /// Player combat carrier: cause-tagged hits, i-frames, hurt flash, and the
    /// one decaying hostile knockback impulse (S06). Ephemeral — never saved.
    pub combat: PlayerCombat,
}

/// Thin player-side wrapper over the shared box-vs-solid predicate.
///
/// The epsilon/flooring rule lives in [`crate::world::collision::solid_overlaps`]
/// so the player and every mob mover cannot drift apart. This wrapper keeps
/// the existing player-sized [`Aabb`] call sites unchanged.
fn collides_with_world<F: Fn(i32, i32, i32) -> BlockId>(aabb: Aabb, get_block: &F) -> bool {
    crate::world::collision::solid_overlaps(aabb.min, aabb.max, get_block)
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
            movement: MovementRules::default(),
            // Fresh players stay Creative so every pre-M008 CLI demo, smoke, and
            // physics test keeps its flying default. `new_with_mode` (and later
            // session restore) overwrites both fields explicitly.
            survival: SurvivalState::full(),
            mode: GameMode::Creative,
            selected_slot: 0,
            inventory: Inventory::empty(),
            player_grid: CraftingGrid::new(GridKind::Player),
            table_grid: CraftingGrid::new(GridKind::Table),
            fall: FallState::default(),
            combat: PlayerCombat::default(),
        }
    }

    /// Builds a fresh player in `mode`.
    ///
    /// Starts from [`Player::new`] so the position/velocity/inventory/grid
    /// defaults are identical to the Creative constructor, then applies the
    /// mode rule. A Survival player walks (fly toggle off); a Creative player
    /// keeps the inherited flying default.
    pub fn new_with_mode(position: Vec3, mode: GameMode) -> Self {
        let mut player = Self::new(position);
        player.set_mode(mode);
        player
    }

    /// The effective game mode.
    pub const fn mode(&self) -> GameMode {
        self.mode
    }

    /// Sets the effective mode.
    ///
    /// Survival structurally forces the fly toggle off; Creative does not
    /// re-enable it, so switching Creative -> Survival -> Creative leaves the
    /// player walking until it explicitly turns flight back on.
    pub fn set_mode(&mut self, mode: GameMode) {
        self.mode = mode;
        if !mode.allows_fly() {
            self.fly_mode = false;
        }
        // A mode change never carries an in-flight fall or spawn immunity.
        self.fall.clear();
        // A mode change must not carry a hostile impulse or i-frame window.
        self.combat.reset();
    }

    /// Whether the player can actually fly right now.
    ///
    /// Only a Creative mode with the toggle on passes. Survival is rejected by
    /// `GameMode::allows_fly`, so assigning `fly_mode = true` by hand (or a
    /// future decode bug) still cannot make a Survival player fly.
    pub const fn can_fly(&self) -> bool {
        self.fly_mode && self.mode.allows_fly()
    }

    /// Toggles flight for a mode that allows it.
    ///
    /// Returns `false` and mutates nothing in Survival. In Creative it flips the
    /// toggle, zeroes the velocity so the mode switch cannot carry momentum
    /// into (or out of) a different movement rule, and returns `true`.
    pub fn toggle_fly(&mut self) -> bool {
        if !self.mode.allows_fly() {
            return false;
        }
        self.fly_mode = !self.fly_mode;
        self.velocity = Vec3::ZERO;
        // Entering or leaving flight drops any in-flight fall measurement, but
        // a fresh-spawn immunity survives an honest mode toggle.
        self.fall.clear_pending();
        true
    }

    /// Returns the one-shot dry-land fall distance latched by the last landing.
    ///
    /// The value is cleared on read so a single fall can never be damaged
    /// twice. Water contact and flight leave no latch behind.
    pub fn take_landing_fall(&mut self) -> f32 {
        let distance = self.fall.landing_distance;
        self.fall.landing_distance = 0.0;
        distance
    }

    /// Arms the first-contact spawn immunity used by fresh Survival sessions
    /// and respawn (S04). Any in-flight fall is discarded.
    pub fn arm_spawn_immunity(&mut self) {
        self.fall.clear_pending();
        self.fall.spawn_immune = true;
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

    /// Grants a Creative palette stack into the hotbar from the session
    /// registry (MEM190/MEM193).
    ///
    /// The authored stack limit comes from [`ContentRegistry::stack_limit`],
    /// the item is built through [`Item::new_with`] (so pack tools start at
    /// full authored durability and pack items respect their authored cap),
    /// and exactly one hotbar slot is written. The granted slot is selected so
    /// the content is immediately placeable or usable. Unknown and
    /// placeholder identities fail with [`ItemError::UnknownContent`] without
    /// mutating the inventory.
    pub fn grant_palette_item_with(
        &mut self,
        registry: &ContentRegistry,
        id: ItemId,
    ) -> Result<(usize, Item), ItemError> {
        let limit = registry.stack_limit(id).ok_or(ItemError::UnknownContent)?;
        let item = Item::new_with(registry, id, limit)?;
        let slot = self.inventory.grant_hotbar(item, self.selected_slot);
        self.select_slot(slot);
        Ok((slot, item))
    }

    /// Collects one broken block into the inventory (hotbar first, then main).
    pub fn collect_block(&mut self, block: BlockId) -> Option<CollectOutcome> {
        self.inventory.collect_block(block, 1)
    }

    /// Collects an authored drop under a registry-supplied stack limit.
    pub fn collect_limited(
        &mut self,
        id: ItemId,
        amount: u16,
        limit: u16,
    ) -> Option<CollectOutcome> {
        self.inventory.collect_limited(id, amount, limit)
    }

    /// Collects a registry-allocated drop (pack item or pack tool).
    ///
    /// Thin forwarder over [`Inventory::collect_limited_with`]: the identity is
    /// constructed through the session registry, so a non-stackable pack tool
    /// can land in the freed hotbar slot under its authored identity.
    pub fn collect_limited_with(
        &mut self,
        registry: &ContentRegistry,
        id: ItemId,
        amount: u16,
        limit: u16,
    ) -> Option<CollectOutcome> {
        self.inventory
            .collect_limited_with(registry, id, amount, limit)
    }

    /// Inserts one whole item stack, all-or-nothing.
    ///
    /// Thin forwarder over [`Inventory::insert`]: `true` only when every unit
    /// fit, `false` with no slot mutated otherwise. World pickup uses this for
    /// the oak-leaf apple bonus; M008 has no item entities or spill, so a full
    /// inventory simply loses the bonus instead of duplicating or dropping it
    /// on the ground.
    pub fn collect_item(&mut self, item: Item) -> bool {
        self.inventory.insert(item).is_ok()
    }

    /// Best-effort batch collection in hotbar-then-main order.
    ///
    /// Thin forwarder over [`Inventory::collect_items`]. M009 mob drops use
    /// this instead of [`Player::collect_item`]: a full inventory accepts what
    /// fits, reports the overflow as [`CollectBatchOutcome::lost`], and never
    /// touches the cursor or either crafting grid.
    pub fn collect_items(&mut self, items: &[Item]) -> CollectBatchOutcome {
        self.inventory.collect_items(items)
    }

    /// Consumes exactly one item from the selected hotbar slot.
    pub fn consume_selected(&mut self) -> Option<Item> {
        self.inventory.consume_one(self.selected_slot)
    }

    /// Consumes exactly one unit of `id` from the first matching slot.
    ///
    /// Thin forwarder over [`Inventory::consume_item`]. A bow lives in the
    /// selected hotbar slot, so its ammo cannot be spent by slot index: this
    /// primitive spends a projectile identity from any of the 36 slots in
    /// hotbar-then-main order and never touches the cursor or either grid.
    /// Returns the consumed unit and the slot it came from, or `None` with no
    /// mutation when the identity is absent.
    pub fn consume_item(&mut self, id: ItemId) -> Option<(usize, Item)> {
        self.inventory.consume_item(id)
    }

    /// Completes one hold-to-eat meal: consumes exactly one `expected` item
    /// from `slot` and restores its food value, returning the hunger actually
    /// applied.
    ///
    /// This is the caller-owned completion half of
    /// [`crate::player::eating::EatingState`]. Every guard runs before any
    /// mutation, so a stale completion (slot switched, item picked up, hunger
    /// already full) is a fail-closed `None` with the inventory and vitals
    /// untouched:
    ///
    /// - `slot` must still be the selected hotbar slot;
    /// - the selected item identity must equal `expected`;
    /// - hunger must be below [`MAX_HUNGER`];
    /// - `expected` must be a food identity.
    ///
    /// One successful call consumes exactly one unit, never a whole stack,
    /// and `SurvivalState::restore_hunger` reports the applied amount after
    /// the cap.
    pub fn eat_selected(&mut self, slot: usize, expected: ItemId) -> Option<u16> {
        self.eat_selected_resolved(slot, expected, food_value(expected)?)
    }

    /// Registry-aware completion half of the hold-to-eat flow.
    ///
    /// Mirrors [`Player::eat_selected`] exactly — selected-slot guard,
    /// identity guard, hunger guard, one-unit consume and capped restore — but
    /// resolves authored nutrition through `registry`, so pack food restores
    /// its authored value while built-in food stays byte-identical. Every
    /// guard still runs before any mutation, making a stale completion a
    /// fail-closed `None`.
    pub fn eat_selected_with(
        &mut self,
        registry: &ContentRegistry,
        slot: usize,
        expected: ItemId,
    ) -> Option<u16> {
        self.eat_selected_with_rules(registry, &EatingRules::default(), slot, expected)
    }

    /// Rules-aware completion half of the hold-to-eat flow.
    ///
    /// Mirrors [`Player::eat_selected_with`] exactly, but built-in food
    /// restores the tuned [`EatingRules`] amount. Pack-authored nutrition
    /// still wins over the rules, matching [`EatingTarget::new_with_rules`],
    /// so a meal can never restore a different amount than the target
    /// displayed. Every guard still runs before any mutation, making a stale
    /// completion a fail-closed `None`.
    pub fn eat_selected_with_rules(
        &mut self,
        registry: &ContentRegistry,
        rules: &EatingRules,
        slot: usize,
        expected: ItemId,
    ) -> Option<u16> {
        self.eat_selected_resolved(
            slot,
            expected,
            food_value_with_registry_rules(registry, expected, rules)?,
        )
    }

    /// Shared guarded consume used by both eating entry points.
    fn eat_selected_resolved(
        &mut self,
        slot: usize,
        expected: ItemId,
        restore: u16,
    ) -> Option<u16> {
        if slot != self.selected_slot || self.survival.hunger() >= MAX_HUNGER {
            return None;
        }
        if self.selected_item()?.id() != expected {
            return None;
        }
        // The slot is the selected slot and holds `expected`, so this cannot
        // fail; the `?` keeps a hypothetical decode bug fail-closed before the
        // hunger restore runs.
        let consumed = self.inventory.consume_one(slot)?;
        debug_assert_eq!(consumed.id(), expected);
        Some(self.survival.restore_hunger(restore))
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

    /// Whether every item in `items` fits into the 36 inventory slots.
    ///
    /// Thin forwarder over [`Inventory::can_insert_all`]: the cursor and both
    /// crafting grids are never counted as spill capacity.
    pub fn can_insert_items(&self, items: &[Item]) -> bool {
        self.inventory.can_insert_all(items)
    }

    /// Inserts every item completely or none at all.
    ///
    /// Thin forwarder over [`Inventory::insert_all`]. The cursor and both
    /// crafting grids are untouched on success and on failure.
    pub fn insert_items(&mut self, items: &[Item]) -> Result<(), InventoryError> {
        self.inventory.insert_all(items)
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

    /// Whole-stack left-click between one chest slot and the shared cursor.
    ///
    /// Thin forwarder over [`Container::click_slot`]. The player aggregate owns
    /// the only cursor, so this is the single UI entry point for moving items
    /// in or out of a chest; both grids and the 36 inventory slots are
    /// untouched.
    pub fn click_container_slot(
        &mut self,
        container: &mut Container,
        index: usize,
    ) -> Result<(), InventoryError> {
        container.click_slot(&mut self.inventory, index)
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

    /// Crafts one result against `registry`.
    ///
    /// The registry-aware forwarder used by the production inventory click
    /// seam (D196/MEM512): pack recipes match, disabled built-ins stay absent,
    /// and the crafted stack is built from registry definitions.
    pub fn take_craft_result_with(
        &mut self,
        registry: &ContentRegistry,
        kind: GridKind,
    ) -> Result<Item, CraftError> {
        match kind {
            GridKind::Player => self
                .player_grid
                .take_result_with(registry, &mut self.inventory),
            GridKind::Table => self
                .table_grid
                .take_result_with(registry, &mut self.inventory),
        }
    }

    /// Read-only recipe preview for the matching grid. Never mutates.
    pub fn craft_preview(&self, kind: GridKind) -> Option<RecipePreview> {
        match kind {
            GridKind::Player => self.player_grid.preview(),
            GridKind::Table => self.table_grid.preview(),
        }
    }

    /// Read-only registry-aware recipe preview for the matching grid.
    pub fn craft_preview_with(
        &self,
        registry: &ContentRegistry,
        kind: GridKind,
    ) -> Option<RecipePreview> {
        match kind {
            GridKind::Player => self.player_grid.preview_with(registry),
            GridKind::Table => self.table_grid.preview_with(registry),
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

        if self.can_fly() {
            let speed = if input.sprint {
                self.movement.fly_sprint_speed
            } else {
                self.movement.fly_speed
            };
            let mut vel = move_dir * speed;
            if input.jump_or_up {
                vel.y += speed * self.movement.fly_vertical_factor;
            }
            if input.crouch_or_down {
                vel.y -= speed * self.movement.fly_vertical_factor;
            }
            self.velocity = vel;
            self.position += self.velocity * dt;
            self.on_ground = false;
            // Flight is never a fall: drop any in-flight measurement.
            self.fall.clear_pending();
            return;
        }

        let start_y = self.position.y;

        // Submersion is derived from the already-in-scope `get_block` closure —
        // no signature change and no new call-site parameter. Sampling the feet
        // block catches a wading body, and the mid-body block keeps a body whose
        // feet have just cleared the surface in the water branch until it is
        // actually out of the water (a generated ocean fills every column below
        // `SEA_LEVEL`).
        let body_x = self.position.x.floor() as i32;
        let body_z = self.position.z.floor() as i32;
        let feet_block_y = (self.position.y + 1e-4).floor() as i32;
        let mid_block_y = (self.position.y + PLAYER_HEIGHT * 0.5).floor() as i32;
        let submerged = get_block(body_x, feet_block_y, body_z).is_liquid()
            || get_block(body_x, mid_block_y, body_z).is_liquid();

        // Crouching deliberately uses the sprint speed too, matching the
        // pre-config behavior.
        let mut walk_speed = if input.sprint || input.crouch_or_down {
            self.movement.sprint_speed
        } else {
            self.movement.walk_speed
        };
        if submerged {
            walk_speed *= self.movement.swim_walk_speed_mult;
        }
        self.velocity.x = move_dir.x * walk_speed;
        self.velocity.z = move_dir.z * walk_speed;
        // Hostile knockback is integrated *after* the input assignment above.
        // Input reassigns `velocity.x/z` every frame, so this is deliberately
        // not a second owner of the velocity: the impulse itself lives only in
        // `combat.knockback`, is never persisted, and has no vertical
        // component so the player is never pushed into the floor or ceiling.
        self.velocity.x += self.combat.knockback().x;
        self.velocity.z += self.combat.knockback().z;

        // Swim branch of walk mode (D087). Honesty note: this asserts mechanics
        // only (positions, sink/rise, on_ground). Fall distance is measured by
        // the dry-land bookkeeping after the sub-step loop below; the damage,
        // hunger, and air rules live in `player::survival` and are applied by
        // `WorldSession::tick_survival` (S04). Water contact cancels a fall.
        if submerged {
            // D087: no buoyant surface lock. A submerged walk-mode body is
            // dragged vertically and then pulled down by gravity while
            // `jump_or_up` is released; holding it applies the rise acceleration
            // instead. Both directions clamp to their terminal swim speeds, so an
            // idle body sinks away from the surface and a held Jump rises back up.
            let drag = (1.0 - self.movement.swim_vertical_drag * dt).clamp(0.0, 1.0);
            self.velocity.y *= drag;
            let vertical_accel = if input.jump_or_up {
                self.movement.swim_rise_accel
            } else {
                -self.movement.swim_sink_accel
            };
            self.velocity.y = (self.velocity.y + vertical_accel * dt).clamp(
                -self.movement.swim_max_sink_speed,
                self.movement.swim_rise_speed,
            );
            // A body in water is never standing: no on-ground land jump of 9.0.
            // The sub-step Y resolution below may still turn this true, but only
            // on a solid block (the seabed), never on the liquid surface.
            self.on_ground = false;
        } else {
            if self.on_ground && input.jump_or_up {
                self.velocity.y = self.movement.jump_velocity;
                self.on_ground = false;
            }

            self.velocity.y = (self.velocity.y - self.movement.gravity * dt)
                .max(-self.movement.terminal_velocity);
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

        // Fall measurement (dry land only). Physics owns the distance; the
        // survival runtime owns the damage. Re-sample submersion at the new
        // position because the body may have entered water during this step.
        let body_x_after = self.position.x.floor() as i32;
        let body_z_after = self.position.z.floor() as i32;
        let feet_block_y_after = (self.position.y + 1e-4).floor() as i32;
        let mid_block_y_after = (self.position.y + PLAYER_HEIGHT * 0.5).floor() as i32;
        let submerged_after = get_block(body_x_after, feet_block_y_after, body_z_after).is_liquid()
            || get_block(body_x_after, mid_block_y_after, body_z_after).is_liquid();

        if submerged || submerged_after {
            // Water contact counts as the first contact: cancel the fall and
            // spend the fresh-spawn immunity.
            self.fall.clear_pending();
            self.fall.spawn_immune = false;
        } else if self.on_ground {
            let peak = self.fall.peak_y.take();
            if self.fall.spawn_immune {
                // Fresh spawn/respawn: discard the first dry landing whole.
                self.fall.spawn_immune = false;
            } else if let Some(peak) = peak {
                self.fall.landing_distance = (peak - self.position.y).max(0.0);
            }
        } else {
            // A ledge walk-off keeps its takeoff height, and a jump still
            // peaks at the apex instead of only recording the final height.
            self.fall.peak_y = Some(
                self.fall
                    .peak_y
                    .unwrap_or(start_y)
                    .max(start_y)
                    .max(self.position.y),
            );
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
    fn collect_item_is_all_or_nothing() {
        let apple = Item::new(ItemId::Material(Material::Apple), 1).expect("valid apple");
        let mut player = Player::new(Vec3::ZERO);
        assert!(
            player.collect_item(apple),
            "an empty inventory fits an apple"
        );
        assert_eq!(player.inventory().slot(0), Some(apple));

        let stone = block_item(BlockId::Stone, 64);
        let slots = [Some(stone); INVENTORY_SLOTS];
        let mut player = Player::with_inventory(
            Vec3::ZERO,
            Inventory::from_parts(slots, None),
            CraftingGrid::new(GridKind::Player),
            CraftingGrid::new(GridKind::Table),
        );
        let before = player.inventory().clone();

        assert!(
            !player.collect_item(apple),
            "one free slot filled by 64 stone cannot take an apple"
        );
        assert_eq!(
            player.inventory(),
            &before,
            "a rejected item leaves every slot untouched"
        );
    }

    #[test]
    fn collect_items_forwarder_reports_acceptance_and_leaves_cursor_and_grids_alone() {
        // The forwarder really reaches the aggregate: an empty player accepts
        // the whole batch in hotbar order.
        let mut player = Player::new(Vec3::ZERO);
        let sticks = stick_item(3);
        let apple = apple_item(1);
        let outcome = player.collect_items(&[sticks, apple]);
        assert_eq!(outcome, CollectBatchOutcome::new(4, 0));
        assert!(!outcome.lost_any());
        assert_eq!(player.inventory().slot(0), Some(sticks));
        assert_eq!(player.inventory().slot(1), Some(apple));

        // A deliberately full player: 36 tool slots and a tool on the cursor.
        // Nothing can merge, so the batch is entirely lost and every other
        // storage surface must stay byte-identical.
        let mut player_grid = CraftingGrid::new(GridKind::Player);
        player_grid
            .set(3, Some(stick_item(2)))
            .expect("player grid cell");
        let mut table_grid = CraftingGrid::new(GridKind::Table);
        table_grid
            .set(15, Some(block_item(BlockId::Stone, 5)))
            .expect("table grid cell");
        let mut player = Player::with_inventory(
            Vec3::ZERO,
            full_tool_inventory_with_cursor(),
            player_grid,
            table_grid,
        );
        let inventory_before = player.inventory().clone();
        let cursor_before = player.inventory().cursor();
        let player_grid_before = player.player_grid().clone();
        let table_grid_before = player.table_grid().clone();

        let outcome = player.collect_items(&[stick_item(10), apple_item(1)]);
        assert_eq!(outcome, CollectBatchOutcome::new(0, 11));
        assert!(outcome.lost_any());
        assert_eq!(player.inventory(), &inventory_before);
        assert_eq!(player.inventory().cursor(), cursor_before);
        assert_eq!(player.player_grid(), &player_grid_before);
        assert_eq!(player.table_grid(), &table_grid_before);
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

    use crate::player::inventory::{
        ItemId, Material, RecipeId, ToolKind, ToolTier, CHEST_SLOTS, INVENTORY_SLOTS,
        MAX_STACK_COUNT,
    };

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

    // -- Chest container / batch insert forwarders ---------------------------

    fn tool_item(kind: ToolKind, tier: ToolTier) -> Item {
        Item::new(ItemId::Tool { kind, tier }, 1).expect("valid test tool")
    }

    /// 36 non-stackable tool slots plus a tool on the cursor. Every inventory
    /// slot is occupied, so no item can be inserted without merging.
    fn full_tool_inventory_with_cursor() -> Inventory {
        let mut slots = [None; INVENTORY_SLOTS];
        for (index, slot) in slots.iter_mut().enumerate() {
            let kind = match index % 3 {
                0 => ToolKind::Pickaxe,
                1 => ToolKind::Axe,
                _ => ToolKind::Shovel,
            };
            let tier = match index % 2 {
                0 => ToolTier::Wood,
                _ => ToolTier::Iron,
            };
            *slot = Some(tool_item(kind, tier));
        }
        Inventory::from_parts(slots, Some(tool_item(ToolKind::Hoe, ToolTier::Stone)))
    }

    fn container_units(container: &Container) -> u32 {
        container
            .slots()
            .iter()
            .flatten()
            .map(|item| u32::from(item.count()))
            .sum()
    }

    #[test]
    fn insert_items_moves_a_batch_without_touching_cursor_or_grids() {
        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(block_item(BlockId::Stone, 900));
        let inventory = Inventory::from_parts(slots, Some(stick_item(7)));
        let mut player_grid = CraftingGrid::new(GridKind::Player);
        player_grid
            .set(3, Some(stick_item(2)))
            .expect("player grid cell");
        let mut table_grid = CraftingGrid::new(GridKind::Table);
        table_grid
            .set(15, Some(block_item(BlockId::OakPlanks, 2)))
            .expect("table grid cell");
        let mut player = Player::with_inventory(Vec3::ZERO, inventory, player_grid, table_grid);

        let cursor_before = player.inventory().cursor();
        let player_grid_before = player.player_grid().clone();
        let table_grid_before = player.table_grid().clone();

        let items = [block_item(BlockId::Stone, 50), stick_item(3)];
        assert!(player.can_insert_items(&items));
        player.insert_items(&items).expect("both items fit");

        assert_eq!(
            player.inventory().slot(0).map(|item| item.count()),
            Some(950)
        );
        assert_eq!(player.inventory().slot(1), Some(stick_item(3)));
        assert_eq!(
            player.inventory().cursor(),
            cursor_before,
            "the cursor is not a batch destination"
        );
        assert_eq!(player.player_grid(), &player_grid_before);
        assert_eq!(player.table_grid(), &table_grid_before);
    }

    #[test]
    fn insert_items_failure_leaves_slots_cursor_and_both_grids_unchanged() {
        let mut player_grid = CraftingGrid::new(GridKind::Player);
        player_grid
            .set(0, Some(block_item(BlockId::OakPlanks, 4)))
            .expect("player grid cell");
        let mut table_grid = CraftingGrid::new(GridKind::Table);
        table_grid
            .set(5, Some(stick_item(3)))
            .expect("table grid cell");
        let mut player = Player::with_inventory(
            Vec3::ZERO,
            full_tool_inventory_with_cursor(),
            player_grid,
            table_grid,
        );

        let before_inventory = player.inventory().clone();
        let before_player_grid = player.player_grid().clone();
        let before_table_grid = player.table_grid().clone();

        let items = [block_item(BlockId::Chest, 1), stick_item(1)];
        assert!(!player.can_insert_items(&items));
        assert_eq!(player.insert_items(&items), Err(InventoryError::NoRoom));
        assert_eq!(player.inventory(), &before_inventory);
        assert_eq!(player.player_grid(), &before_player_grid);
        assert_eq!(
            player.table_grid(),
            &before_table_grid,
            "a rejected batch never spills into the crafting grids"
        );
    }

    #[test]
    fn cursor_and_grid_contents_do_not_create_spill_capacity() {
        let mut slots = [None; INVENTORY_SLOTS];
        for slot in slots.iter_mut() {
            *slot = Some(block_item(BlockId::Brick, MAX_STACK_COUNT));
        }
        // Cursor and both grids hold the identity being inserted; none of
        // them is part of the 36-slot capacity calculation.
        let mut player_grid = CraftingGrid::new(GridKind::Player);
        player_grid
            .set(0, Some(block_item(BlockId::Stone, MAX_STACK_COUNT)))
            .expect("player grid cell");
        let mut table_grid = CraftingGrid::new(GridKind::Table);
        table_grid
            .set(15, Some(block_item(BlockId::Stone, MAX_STACK_COUNT)))
            .expect("table grid cell");
        let mut player = Player::with_inventory(
            Vec3::ZERO,
            Inventory::from_parts(slots, Some(block_item(BlockId::Stone, 5))),
            player_grid,
            table_grid,
        );

        let before_inventory = player.inventory().clone();
        let before_player_grid = player.player_grid().clone();
        let before_table_grid = player.table_grid().clone();

        let stone = [block_item(BlockId::Stone, 1)];
        assert!(
            !player.can_insert_items(&stone),
            "a matching cursor stack and grid cells are not inventory capacity"
        );
        assert_eq!(player.insert_items(&stone), Err(InventoryError::NoRoom));
        assert_eq!(player.inventory(), &before_inventory);
        assert_eq!(player.player_grid(), &before_player_grid);
        assert_eq!(player.table_grid(), &before_table_grid);
    }

    #[test]
    fn container_slot_click_moves_items_through_the_shared_cursor() {
        let mut player = player_with_slots([(0, block_item(BlockId::Stone, 4))]);
        let mut container = Container::from_slots([None; CHEST_SLOTS]);
        let units_before = player_units(&player) + container_units(&container);

        player
            .click_inventory_slot(0)
            .expect("pick the stone into the cursor");
        assert_eq!(
            player.inventory().cursor(),
            Some(block_item(BlockId::Stone, 4))
        );

        player
            .click_container_slot(&mut container, 5)
            .expect("cursor into the chest");
        assert_eq!(container.slot(5), Some(block_item(BlockId::Stone, 4)));
        assert_eq!(player.inventory().cursor(), None);
        assert_eq!(
            player_units(&player) + container_units(&container),
            units_before,
            "player -> chest conserves items"
        );

        player
            .click_container_slot(&mut container, 5)
            .expect("chest back onto the cursor");
        assert_eq!(container.slot(5), None);
        assert_eq!(
            player.inventory().cursor(),
            Some(block_item(BlockId::Stone, 4))
        );
        assert_eq!(
            player_units(&player) + container_units(&container),
            units_before,
            "chest -> player conserves items"
        );
    }

    #[test]
    fn container_slot_click_out_of_bounds_preserves_chest_and_player() {
        let mut player = player_with_slots([(0, block_item(BlockId::Stone, 2))]);
        player
            .click_inventory_slot(0)
            .expect("pick the stone into the cursor");
        let mut slots = [None; CHEST_SLOTS];
        slots[0] = Some(stick_item(3));
        let mut container = Container::from_slots(slots);

        let before_container = container.clone();
        let before_inventory = player.inventory().clone();
        let before_player_grid = player.player_grid().clone();
        let before_table_grid = player.table_grid().clone();

        assert_eq!(
            player.click_container_slot(&mut container, CHEST_SLOTS),
            Err(InventoryError::OutOfBounds)
        );
        assert_eq!(
            player.click_container_slot(&mut container, usize::MAX),
            Err(InventoryError::OutOfBounds)
        );
        assert_eq!(container, before_container);
        assert_eq!(player.inventory(), &before_inventory);
        assert_eq!(player.player_grid(), &before_player_grid);
        assert_eq!(player.table_grid(), &before_table_grid);
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

    /// Solid floor at `floor_y` with open air above. Used by the fall tests.
    fn dry_land_at(floor_y: i32) -> impl Fn(i32, i32, i32) -> BlockId {
        move |_x, y, _z| {
            if y < floor_y {
                BlockId::Stone
            } else {
                BlockId::Air
            }
        }
    }

    /// Runs gravity-only frames until the body lands, bounded to 10 game
    /// seconds at 60 Hz.
    fn fall_to_ground(player: &mut Player, floor_y: i32) {
        let idle = InputState::default();
        for frame in 0..600 {
            player.update(1.0 / 60.0, &idle, dry_land_at(floor_y));
            if player.on_ground {
                return;
            }
            assert!(
                player.position.y > floor_y as f32 - 1.0,
                "fall fell through the floor at frame {frame}: y={}",
                player.position.y
            );
        }
        panic!("body never landed on floor y={floor_y}");
    }

    #[test]
    fn dry_land_falls_latch_distance_and_map_to_survival_damage() {
        for (drop, expected_damage) in [(3.0_f32, 0_u16), (4.0, 1), (5.0, 2)] {
            let mut player =
                Player::new_with_mode(Vec3::new(0.5, 5.0 + drop, 0.5), GameMode::Survival);
            fall_to_ground(&mut player, 5);
            let distance = player.take_landing_fall();
            assert!(
                (distance - drop).abs() < 1e-3,
                "a {drop}-block drop must latch {drop}, got {distance}"
            );
            assert_eq!(
                crate::player::survival::fall_damage(distance),
                expected_damage,
                "a {drop}-block drop must map to {expected_damage} damage"
            );
            assert_eq!(
                player.take_landing_fall(),
                0.0,
                "the landing latch must be consumed exactly once"
            );
        }
    }

    #[test]
    fn normal_land_jump_latches_less_than_the_safe_drop() {
        let mut player = Player::new_with_mode(Vec3::new(0.5, 5.0, 0.5), GameMode::Survival);
        player.on_ground = true;
        let jump = InputState {
            jump_or_up: true,
            ..Default::default()
        };
        player.update(1.0 / 60.0, &jump, dry_land_at(5));
        let idle = InputState::default();
        for _ in 0..600 {
            player.update(1.0 / 60.0, &idle, dry_land_at(5));
            if player.on_ground {
                break;
            }
        }
        assert!(player.on_ground, "the jump must come back down");
        let distance = player.take_landing_fall();
        assert!(
            distance > 0.0 && distance < 3.0,
            "a 9.0 m/s jump must stay under the 3-block safe drop, got {distance}"
        );
    }

    #[test]
    fn water_entry_cancels_the_fall_and_spends_spawn_immunity() {
        let mut player = Player::new_with_mode(
            Vec3::new(0.5, TEST_SEA_LEVEL as f32 + 10.0, 0.5),
            GameMode::Survival,
        );
        player.arm_spawn_immunity();
        let idle = InputState::default();
        let mut entered = false;
        for _ in 0..600 {
            player.update(1.0 / 60.0, &idle, water_column);
            if player.position.y < TEST_SEA_LEVEL as f32 {
                entered = true;
                break;
            }
        }
        assert!(entered, "the body must enter the water column");
        assert_eq!(
            player.take_landing_fall(),
            0.0,
            "water contact must cancel the fall"
        );
        assert!(
            !player.fall.spawn_immune,
            "water contact counts as the first contact and spends immunity"
        );
        assert!(player.fall.peak_y.is_none());
    }

    #[test]
    fn spawn_immunity_discards_only_the_first_dry_landing() {
        let mut player = Player::new_with_mode(Vec3::new(0.5, 9.0, 0.5), GameMode::Survival);
        player.arm_spawn_immunity();
        fall_to_ground(&mut player, 5);
        assert_eq!(
            player.take_landing_fall(),
            0.0,
            "the armed first landing must be discarded"
        );
        assert!(!player.fall.spawn_immune, "immunity is a one-shot");

        player.position.y = 9.0;
        player.velocity = Vec3::ZERO;
        player.on_ground = false;
        fall_to_ground(&mut player, 5);
        let distance = player.take_landing_fall();
        assert!(
            (distance - 4.0).abs() < 1e-3,
            "the second 4-block fall must be measured, got {distance}"
        );
    }

    #[test]
    fn restored_player_without_armed_immunity_takes_the_first_fall() {
        // Session restore never arms immunity: a saved mid-air fall must hurt.
        let mut player = Player::with_inventory(
            Vec3::new(0.5, 9.0, 0.5),
            Inventory::empty(),
            CraftingGrid::new(GridKind::Player),
            CraftingGrid::new(GridKind::Table),
        );
        player.set_mode(GameMode::Survival);
        assert!(!player.fall.spawn_immune);
        fall_to_ground(&mut player, 5);
        let distance = player.take_landing_fall();
        assert!(
            (distance - 4.0).abs() < 1e-3,
            "an unarmed restored player must take the first 4-block fall, got {distance}"
        );
    }

    #[test]
    fn toggle_fly_and_set_mode_drop_a_pending_fall() {
        let mut player = Player::new_with_mode(Vec3::new(0.5, 40.0, 0.5), GameMode::Survival);
        let idle = InputState::default();
        for _ in 0..10 {
            player.update(1.0 / 60.0, &idle, |_, _, _| BlockId::Air);
        }
        assert!(player.fall.peak_y.is_some(), "a dry fall must build a peak");

        player.set_mode(GameMode::Creative);
        assert!(
            player.fall.peak_y.is_none(),
            "set_mode must clear the pending peak"
        );
        assert_eq!(
            player.fall.landing_distance, 0.0,
            "set_mode must clear a latched landing"
        );

        for _ in 0..10 {
            player.update(1.0 / 60.0, &idle, |_, _, _| BlockId::Air);
        }
        assert!(player.fall.peak_y.is_some());
        assert!(player.toggle_fly(), "Creative must allow the fly toggle");
        assert!(
            player.fall.peak_y.is_none(),
            "toggling flight must clear the pending peak"
        );

        player.fall.landing_distance = 7.0;
        assert!(player.toggle_fly());
        assert_eq!(
            player.fall.landing_distance, 0.0,
            "toggling flight must clear a latched landing"
        );
    }

    #[test]
    fn swim_idle_submerged_player_sinks_away_from_the_surface() {
        // D087: no automatic surface lock. Released just under the surface with
        // no input at all, the body must descend under drag plus gravity instead
        // of hovering at the surface band.
        let mut player = Player::new(Vec3::new(0.5, TEST_SEA_LEVEL as f32 - 0.1, 0.5));
        player.fly_mode = false;
        let input = InputState::default();
        let start_y = player.position.y;
        let mut min_y = start_y;
        for frame in 0..120 {
            player.update(1.0 / 60.0, &input, water_column);
            min_y = min_y.min(player.position.y);
            assert!(
                !player.on_ground,
                "an idle sinking body must not touch ground before the seabed: frame {frame}, y={}",
                player.position.y
            );
        }
        assert!(
            start_y - min_y > 8.0,
            "an idle submerged body must sink away from the surface, not float: start {start_y}, lowest {min_y}"
        );
        assert!(
            player.position.y < start_y - 8.0,
            "the body must still be sinking at the end of the window: start {start_y}, end {}",
            player.position.y
        );
        assert!(
            player.velocity.y < 0.0,
            "the idle body must carry downward velocity, got {}",
            player.velocity.y
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
            "holding Space must rise through the column and clear the water surface, y={}",
            player.position.y
        );
    }

    #[test]
    fn swim_vertical_drag_bounds_the_sink_and_rise_speeds() {
        // Sink: idle just under the surface. The per-frame downward step stays
        // far below a teleport and the terminal speed is capped by the constant.
        let mut sinker = Player::new(Vec3::new(0.5, TEST_SEA_LEVEL as f32 - 2.0, 0.5));
        sinker.fly_mode = false;
        let idle = InputState::default();
        let mut max_sink_step = 0.0f32;
        for _ in 0..60 {
            let before = sinker.position.y;
            sinker.update(1.0 / 60.0, &idle, water_column);
            max_sink_step = max_sink_step.max(before - sinker.position.y);
        }
        assert!(
            max_sink_step > 0.0,
            "the idle body must actually move down (max per-frame sink {max_sink_step})"
        );
        assert!(
            max_sink_step <= SWIM_MAX_SINK_SPEED * (1.0 / 60.0) + 1e-3,
            "vertical drag must bound the sink step, got {max_sink_step} blocks/frame (teleport-like)"
        );
        assert!(
            -sinker.velocity.y <= SWIM_MAX_SINK_SPEED + 1e-3,
            "terminal sink speed must stay capped at {SWIM_MAX_SINK_SPEED}, got {}",
            sinker.velocity.y
        );

        // Rise: holding Jump accelerates upward but never past the rise cap.
        let mut riser = Player::new(Vec3::new(0.5, 3.0, 0.5));
        riser.fly_mode = false;
        let rise = InputState {
            jump_or_up: true,
            ..Default::default()
        };
        let mut max_rise_step = 0.0f32;
        for _ in 0..60 {
            let before = riser.position.y;
            riser.update(1.0 / 60.0, &rise, water_column);
            max_rise_step = max_rise_step.max(riser.position.y - before);
        }
        assert!(
            max_rise_step > 0.0,
            "holding Jump must actually lift the body (max per-frame rise {max_rise_step})"
        );
        assert!(
            max_rise_step <= SWIM_RISE_SPEED * (1.0 / 60.0) + 1e-3,
            "vertical drag must bound the rise step, got {max_rise_step} blocks/frame"
        );
        assert!(
            riser.velocity.y > 0.0 && riser.velocity.y <= SWIM_RISE_SPEED + 1e-3,
            "held Jump must produce a bounded upward speed, got {}",
            riser.velocity.y
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
        // water column with no input keeps its velocity (no water drag or gravity).
        let mut flyer = Player::new(Vec3::new(0.5, 5.0, 0.5));
        flyer.fly_mode = true;
        let input = InputState::default();
        for _ in 0..30 {
            flyer.update(1.0 / 60.0, &input, water_column);
        }
        assert_eq!(flyer.position.y, 5.0, "fly mode ignores water physics");
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
        // Dropped exactly at the surface: the body falls into the water and then
        // keeps sinking (D087). Water is not solid, so the surface can never
        // provide ground contact; over this bounded window the body sinks before
        // it can reach the seabed.
        assert!(
            !BlockId::Water.is_solid(),
            "water must stay passable-not-walkable"
        );
        let mut player = Player::new(Vec3::new(0.5, TEST_SEA_LEVEL as f32, 0.5));
        player.fly_mode = false;
        let input = InputState::default();
        let start_y = player.position.y;
        let mut min_y = start_y;
        for frame in 0..120 {
            player.update(1.0 / 60.0, &input, water_column);
            min_y = min_y.min(player.position.y);
            assert!(
                !player.on_ground,
                "a player must never stand on the water surface: frame {frame}, y={}",
                player.position.y
            );
        }
        assert!(
            min_y < start_y - 2.0,
            "dropped at the surface the body must sink before any ground contact: start {start_y}, lowest {min_y}"
        );
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
    /// Honesty note: this S03/D087 acceptance test makes no fall-damage,
    /// health, or drowning claim. Fall distance is measured in `Player::update`
    /// (S04), while the damage/hunger/air rules live in `player::survival` and
    /// are applied by `WorldSession::tick_survival`; water contact cancels a
    /// fall. The swim assertions here cover mechanics only (positions,
    /// sink/rise, `on_ground`).
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

        // Not walkable: released exactly at the surface with no input, the body
        // sinks under drag plus gravity (D087) instead of floating; it never
        // stands on the liquid and (over this bounded window) never reaches the
        // seabed either. Water is not solid, so the surface cannot supply
        // ground contact.
        let mut sinker = Player::new(Vec3::new(0.5, TEST_SEA_LEVEL as f32, 0.5));
        sinker.fly_mode = false;
        let idle = InputState::default();
        let start_y = sinker.position.y;
        let mut lowest = start_y;
        for _ in 0..120 {
            sinker.update(1.0 / 60.0, &idle, water_column);
            lowest = lowest.min(sinker.position.y);
            assert!(
                !sinker.on_ground,
                "water is not walkable: on_ground became true at y={}",
                sinker.position.y
            );
        }
        assert!(
            lowest < start_y - 2.0,
            "an idle body must sink away from the water surface (start {start_y}, lowest {lowest})"
        );
    }

    // -----------------------------------------------------------------------
    // M008: game modes and the structurally gated flight rule
    // -----------------------------------------------------------------------

    #[test]
    fn new_player_is_creative_full_and_flying() {
        let player = Player::new(Vec3::new(0.5, 10.0, 0.5));
        assert_eq!(player.mode(), GameMode::Creative);
        assert_eq!(player.survival, SurvivalState::full());
        assert!(player.fly_mode);
        assert!(player.can_fly());
    }

    #[test]
    fn new_with_mode_survival_walks_with_full_vitals() {
        let player = Player::new_with_mode(Vec3::new(0.5, 10.0, 0.5), GameMode::Survival);
        assert_eq!(player.mode(), GameMode::Survival);
        assert_eq!(player.survival, SurvivalState::full());
        assert!(!player.fly_mode);
        assert!(!player.can_fly());
    }

    #[test]
    fn new_with_mode_creative_flies() {
        let player = Player::new_with_mode(Vec3::new(0.5, 10.0, 0.5), GameMode::Creative);
        assert_eq!(player.mode(), GameMode::Creative);
        assert!(player.fly_mode);
        assert!(player.can_fly());
    }

    #[test]
    fn set_mode_survival_disables_fly() {
        let mut player = Player::new(Vec3::new(0.5, 10.0, 0.5));
        assert!(player.can_fly());
        player.set_mode(GameMode::Survival);
        assert!(!player.fly_mode);
        assert!(!player.can_fly());
    }

    #[test]
    fn set_mode_creative_does_not_re_enable_fly() {
        let mut player = Player::new_with_mode(Vec3::new(0.5, 10.0, 0.5), GameMode::Survival);
        player.set_mode(GameMode::Creative);
        assert_eq!(player.mode(), GameMode::Creative);
        assert!(!player.fly_mode);
        assert!(!player.can_fly());
        // An explicit toggle is still available once the mode allows it.
        assert!(player.toggle_fly());
        assert!(player.can_fly());
    }

    #[test]
    fn toggle_fly_creative_toggles_and_zeroes_velocity() {
        let mut player = Player::new(Vec3::new(0.5, 10.0, 0.5));
        player.velocity = Vec3::new(1.0, -2.0, 3.0);
        assert!(player.toggle_fly());
        assert!(!player.fly_mode);
        assert!(!player.can_fly());
        assert_eq!(player.velocity, Vec3::ZERO);

        player.velocity = Vec3::new(-4.0, 5.0, -6.0);
        assert!(player.toggle_fly());
        assert!(player.fly_mode);
        assert!(player.can_fly());
        assert_eq!(player.velocity, Vec3::ZERO);
    }

    #[test]
    fn toggle_fly_survival_is_a_no_op() {
        let mut player = Player::new_with_mode(Vec3::new(0.5, 10.0, 0.5), GameMode::Survival);
        player.fly_mode = true; // stale hand-set flag: still structurally gated
        player.velocity = Vec3::new(1.0, 2.0, 3.0);
        assert!(!player.toggle_fly());
        assert!(player.fly_mode, "Survival toggle must mutate nothing");
        assert!(!player.can_fly());
        assert_eq!(player.velocity, Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn survival_update_falls_under_gravity_even_with_fly_flag_true() {
        let mut player = Player::new_with_mode(Vec3::new(0.5, 60.0, 0.5), GameMode::Survival);
        player.fly_mode = true;
        let start_y = player.position.y;
        let idle = InputState::default();
        for _ in 0..30 {
            player.update(1.0 / 60.0, &idle, |_, _, _| BlockId::Air);
        }
        assert!(
            player.position.y < start_y - 1.0,
            "Survival must fall, not hover: {start_y} -> {}",
            player.position.y
        );
        assert!(!player.on_ground);
    }

    #[test]
    fn creative_update_still_uses_the_fly_branch() {
        let mut player = Player::new(Vec3::new(0.5, 60.0, 0.5));
        let start_y = player.position.y;
        let up = InputState {
            jump_or_up: true,
            ..Default::default()
        };
        player.update(1.0 / 60.0, &up, |_, _, _| BlockId::Air);
        assert!(
            player.position.y > start_y,
            "Creative flying must rise: {start_y} -> {}",
            player.position.y
        );
        assert!(!player.on_ground);
        // No gravity while flying: idle input leaves the position unchanged.
        let hover_y = player.position.y;
        player.update(1.0 / 60.0, &InputState::default(), |_, _, _| BlockId::Air);
        assert!((player.position.y - hover_y).abs() < 1e-6);
    }

    // -----------------------------------------------------------------------
    // S05-T03: caller-owned hold-to-eat completion
    // -----------------------------------------------------------------------

    fn apple_item(count: u16) -> Item {
        Item::new(ItemId::Material(Material::Apple), count).expect("valid apple stack")
    }

    fn bread_item(count: u16) -> Item {
        Item::new(ItemId::Material(Material::Bread), count).expect("valid bread stack")
    }

    // -----------------------------------------------------------------------
    // S05-T04: authored pack food through the same completion guards
    // -----------------------------------------------------------------------

    /// A pack registry with `mymod:ruby_stew` (nutrition 7, 0.8 s) and a
    /// non-food `mymod:ruby`.
    fn pack_food_registry() -> ContentRegistry {
        use crate::content::spec::{FoodSpec, ItemEntry, PackAsset};
        use crate::content::{build_registry, PackContentSpec, PackIdentity};

        let mut spec = PackContentSpec::default();
        for (id, icon, food) in [
            (
                "mymod:ruby_stew",
                "stew.png",
                Some(FoodSpec {
                    nutrition: 7,
                    eat_seconds: 0.8,
                }),
            ),
            ("mymod:ruby", "ruby.png", None),
        ] {
            spec.items.push(ItemEntry {
                id: id.to_string(),
                display_name: None,
                icon: icon.to_string(),
                stack_limit: 64,
                food,
            });
        }
        for (name, byte) in [("stew.png", 0x11), ("ruby.png", 0x22)] {
            spec.assets.insert(
                name.to_string(),
                PackAsset {
                    rgba: vec![byte; 1024],
                    source_side: 16,
                },
            );
        }
        let pack = (
            PackIdentity {
                folder: "mymod".to_string(),
                version: None,
            },
            spec,
        );
        let (registry, report) = build_registry(ContentRegistry::builtin(), &[pack]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        registry
    }

    fn pack_stew_index(registry: &ContentRegistry) -> u16 {
        registry
            .item_by_name("mymod:ruby_stew")
            .expect("registered pack stew")
            .index
    }

    fn pack_gem_index(registry: &ContentRegistry) -> u16 {
        registry
            .item_by_name("mymod:ruby")
            .expect("registered pack gem")
            .index
    }

    fn pack_stew(registry: &ContentRegistry, count: u16) -> Item {
        Item::new_with(registry, ItemId::Item(pack_stew_index(registry)), count)
            .expect("registered pack stew")
    }

    #[test]
    fn eat_selected_with_consumes_one_stew_and_restores_authored_seven() {
        let registry = pack_food_registry();
        let mut player = food_player(pack_stew(&registry, 2), 10);

        let applied =
            player.eat_selected_with(&registry, 0, ItemId::Item(pack_stew_index(&registry)));

        assert_eq!(applied, Some(7), "authored nutrition drives the restore");
        assert_eq!(player.survival.hunger(), 17);
        assert_eq!(selected_count(&player), Some(1));
        // The second call consumes the last unit and caps at MAX_HUNGER.
        assert_eq!(
            player.eat_selected_with(&registry, 0, ItemId::Item(pack_stew_index(&registry))),
            Some(3),
            "the last stew restores only the capped remainder"
        );
        assert_eq!(player.survival.hunger(), MAX_HUNGER);
    }

    #[test]
    fn eat_selected_with_rules_restores_the_tuned_builtin_amount() {
        let registry = ContentRegistry::builtin();
        let rules = EatingRules {
            apple_hunger: 7,
            ..EatingRules::default()
        };
        let mut player = food_player(apple_item(2), 10);

        let applied =
            player.eat_selected_with_rules(&registry, &rules, 0, ItemId::Material(Material::Apple));

        assert_eq!(applied, Some(7), "the tuned built-in restore applies");
        assert_eq!(player.survival.hunger(), 17);
        assert_eq!(selected_count(&player), Some(1));

        // The default wrapper still restores the frozen four.
        let mut player = food_player(apple_item(1), 10);
        assert_eq!(
            player.eat_selected_with(&registry, 0, ItemId::Material(Material::Apple)),
            Some(4)
        );
        assert_eq!(player.survival.hunger(), 14);
    }

    #[test]
    fn eat_selected_with_rules_keeps_pack_authored_nutrition() {
        let registry = pack_food_registry();
        let rules = EatingRules {
            apple_hunger: 19,
            bread_hunger: 19,
            raw_meat_hunger: 19,
            rotten_flesh_hunger: 19,
            ..EatingRules::default()
        };
        let stew = ItemId::Item(pack_stew_index(&registry));
        let mut player = food_player(pack_stew(&registry, 1), 10);

        let applied = player.eat_selected_with_rules(&registry, &rules, 0, stew);
        assert_eq!(applied, Some(7), "pack nutrition wins over tuned rules");
        assert_eq!(player.survival.hunger(), 17);
        assert_eq!(selected_count(&player), None, "one stew consumed");
    }

    #[test]
    fn eat_selected_with_refuses_full_hunger_without_mutation() {
        let registry = pack_food_registry();
        let mut player = food_player(pack_stew(&registry, 2), MAX_HUNGER);

        assert_eq!(
            player.eat_selected_with(&registry, 0, ItemId::Item(pack_stew_index(&registry))),
            None
        );
        assert_eq!(player.survival.hunger(), MAX_HUNGER);
        assert_eq!(selected_count(&player), Some(2), "nothing was consumed");
    }

    #[test]
    fn eat_selected_with_refuses_non_food_and_unknown_indices() {
        let registry = pack_food_registry();
        let gem = Item::new_with(&registry, ItemId::Item(pack_gem_index(&registry)), 2)
            .expect("registered pack gem");
        let mut player = food_player(gem, 10);
        assert_eq!(
            player.eat_selected_with(&registry, 0, ItemId::Item(pack_gem_index(&registry))),
            None,
            "a non-food pack item is never edible"
        );
        assert_eq!(player.survival.hunger(), 10);
        assert_eq!(selected_count(&player), Some(2));

        let mut player = food_player(pack_stew(&registry, 1), 10);
        assert_eq!(
            player.eat_selected_with(&registry, 0, ItemId::Item(99)),
            None,
            "an unknown index is not food"
        );
        assert_eq!(player.survival.hunger(), 10);
        assert_eq!(selected_count(&player), Some(1));
    }

    #[test]
    fn eat_selected_with_rejects_stale_completions_fail_closed() {
        let registry = pack_food_registry();

        // Slot switched after the hold started.
        let mut player = food_player(pack_stew(&registry, 2), 10);
        assert!(player.select_slot(1));
        assert_eq!(
            player.eat_selected_with(&registry, 0, ItemId::Item(pack_stew_index(&registry))),
            None
        );
        assert_eq!(player.survival.hunger(), 10);
        assert_eq!(
            player.inventory().hotbar(0).map(|item| item.count()),
            Some(2)
        );

        // The held item was picked up before completion.
        let mut player = food_player(pack_stew(&registry, 1), 10);
        player
            .inventory
            .consume_one(0)
            .expect("the selected stew exists");
        assert_eq!(
            player.eat_selected_with(&registry, 0, ItemId::Item(pack_stew_index(&registry))),
            None
        );
        assert_eq!(player.survival.hunger(), 10);
        assert_eq!(selected_count(&player), None);
    }

    /// A Survival player holding `item` in slot 0 at `hunger`.
    ///
    /// `Player::with_inventory` is always Creative, so the mode is set
    /// explicitly before any eating command runs.
    fn food_player(item: Item, hunger: u16) -> Player {
        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(item);
        let mut player = Player::with_inventory(
            Vec3::ZERO,
            Inventory::from_parts(slots, None),
            CraftingGrid::new(GridKind::Player),
            CraftingGrid::new(GridKind::Table),
        );
        player.set_mode(GameMode::Survival);
        player.survival.spend_hunger(MAX_HUNGER - hunger);
        assert!(player.select_slot(0));
        player
    }

    fn selected_count(player: &Player) -> Option<u16> {
        player.selected_item().map(|item| item.count())
    }

    #[test]
    fn eat_selected_consumes_one_apple_and_restores_four_hunger() {
        let mut player = food_player(apple_item(2), 10);

        let applied = player.eat_selected(0, ItemId::Material(Material::Apple));

        assert_eq!(applied, Some(4), "one apple restores exactly 4 hunger");
        assert_eq!(player.survival.hunger(), 14);
        assert_eq!(
            selected_count(&player),
            Some(1),
            "exactly one item consumed"
        );
    }

    #[test]
    fn eat_selected_consumes_one_bread_and_restores_six_hunger() {
        let mut player = food_player(bread_item(2), 10);

        let applied = player.eat_selected(0, ItemId::Material(Material::Bread));

        assert_eq!(applied, Some(6), "one bread restores exactly 6 hunger");
        assert_eq!(player.survival.hunger(), 16);
        assert_eq!(
            selected_count(&player),
            Some(1),
            "exactly one item consumed"
        );
    }

    #[test]
    fn eat_selected_reports_the_capped_restore_at_hunger_nineteen() {
        // 19 is the last hunger value a meal can start from; the restore is
        // reported after the MAX_HUNGER cap rather than as the food's nominal
        // value.
        let mut player = food_player(bread_item(2), MAX_HUNGER - 1);

        let applied = player.eat_selected(0, ItemId::Material(Material::Bread));

        assert_eq!(applied, Some(1));
        assert_eq!(player.survival.hunger(), MAX_HUNGER);
        assert_eq!(selected_count(&player), Some(1));
    }

    #[test]
    fn eat_selected_rejects_a_slot_mismatch_without_mutation() {
        let mut player = food_player(apple_item(2), 10);
        // A second apple elsewhere proves the guard is about the selected
        // slot, not merely about the item existing somewhere.
        let mut slots = *player.inventory().slots();
        slots[1] = Some(apple_item(1));
        player = Player::with_inventory(
            player.position,
            Inventory::from_parts(slots, None),
            CraftingGrid::new(GridKind::Player),
            CraftingGrid::new(GridKind::Table),
        );
        player.set_mode(GameMode::Survival);
        player.survival.spend_hunger(10);
        assert!(player.select_slot(1));

        assert_eq!(
            player.eat_selected(0, ItemId::Material(Material::Apple)),
            None
        );
        assert_eq!(player.survival.hunger(), 10);
        assert_eq!(
            player.inventory().hotbar(0).map(|item| item.count()),
            Some(2)
        );
        assert_eq!(
            player.inventory().hotbar(1).map(|item| item.count()),
            Some(1)
        );
    }

    #[test]
    fn eat_selected_rejects_an_item_mismatch_without_mutation() {
        let mut player = food_player(apple_item(2), 10);

        assert_eq!(
            player.eat_selected(0, ItemId::Material(Material::Bread)),
            None
        );
        assert_eq!(player.survival.hunger(), 10);
        assert_eq!(selected_count(&player), Some(2));
    }

    #[test]
    fn eat_selected_rejects_an_empty_slot_without_mutation() {
        let mut player = Player::with_inventory(
            Vec3::ZERO,
            Inventory::empty(),
            CraftingGrid::new(GridKind::Player),
            CraftingGrid::new(GridKind::Table),
        );
        player.set_mode(GameMode::Survival);
        player.survival.spend_hunger(10);
        assert!(player.select_slot(0));

        assert_eq!(
            player.eat_selected(0, ItemId::Material(Material::Apple)),
            None
        );
        assert_eq!(player.survival.hunger(), 10);
        assert_eq!(selected_count(&player), None);
    }

    #[test]
    fn eat_selected_refuses_full_hunger_without_consuming() {
        let mut player = food_player(apple_item(2), MAX_HUNGER);

        assert_eq!(
            player.eat_selected(0, ItemId::Material(Material::Apple)),
            None
        );
        assert_eq!(player.survival.hunger(), MAX_HUNGER);
        assert_eq!(selected_count(&player), Some(2), "nothing was consumed");
    }

    #[test]
    fn eat_selected_rejects_non_food_even_when_selected() {
        let mut player = food_player(
            Item::new(ItemId::Material(Material::Stick), 2).expect("stick stack"),
            10,
        );

        assert_eq!(
            player.eat_selected(0, ItemId::Material(Material::Stick)),
            None
        );
        assert_eq!(player.survival.hunger(), 10);
        assert_eq!(selected_count(&player), Some(2));
    }

    #[test]
    fn is_moving_tracks_every_movement_key() {
        let input = InputState::default();
        assert!(!input.is_moving(), "an idle input is not moving");

        for index in 0..7 {
            let mut input = InputState::default();
            match index {
                0 => input.forward = true,
                1 => input.backward = true,
                2 => input.left = true,
                3 => input.right = true,
                4 => input.jump_or_up = true,
                5 => input.crouch_or_down = true,
                6 => input.sprint = true,
                _ => unreachable!(),
            }
            assert!(input.is_moving(), "key index {index} must count as moving");
            input.reset();
            assert!(!input.is_moving(), "reset clears every movement key");
        }
    }

    // -----------------------------------------------------------------------
    // S07-T02: Creative palette hotbar grants
    // -----------------------------------------------------------------------

    /// The checked-in reference pack through the same production mapping as
    /// `main.rs::build_session_registry`.
    fn reference_registry() -> ContentRegistry {
        let spec = crate::pack::spec::load_spec_from_dir(std::path::Path::new(
            "tests/fixtures/packs/mymod-ruby",
        ));
        let pack = (
            crate::content::PackIdentity {
                folder: spec.folder.clone(),
                version: spec.version.clone(),
            },
            spec.content.clone(),
        );
        let (registry, report) =
            crate::content::build_registry(ContentRegistry::builtin(), &[pack]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        registry
    }

    fn creative_player() -> Player {
        Player::new_with_mode(Vec3::new(0.5, 10.0, 0.5), GameMode::Creative)
    }

    #[test]
    fn palette_grant_builds_authored_stacks_and_selects_the_slot() {
        let registry = reference_registry();
        let mut player = creative_player();

        let ore = registry
            .block_by_name("mymod:ruby_ore")
            .expect("registered pack ore")
            .id;
        let (slot, item) = player
            .grant_palette_item_with(&registry, ItemId::Block(ore))
            .expect("pack ore is grantable");
        assert_eq!(slot, 0);
        assert_eq!(player.selected_slot(), 0, "the granted slot becomes live");
        assert_eq!(item.count(), 64, "the authored block stack is 64");
        assert_eq!(player.selected_block(), Some(ore));

        let stew = registry
            .item_by_name("mymod:ruby_stew")
            .expect("registered pack stew")
            .index;
        let (slot, item) = player
            .grant_palette_item_with(&registry, ItemId::Item(stew))
            .expect("pack stew is grantable");
        assert_eq!(slot, 1);
        assert_eq!(
            item.count(),
            16,
            "16 is the authored stew limit, not 64 or 999"
        );
        assert_eq!(player.selected_slot(), 1);

        let pickaxe = registry
            .tool_by_name("mymod:ruby_pickaxe")
            .expect("registered pack pickaxe")
            .index;
        let (slot, item) = player
            .grant_palette_item_with(&registry, ItemId::ModTool(pickaxe))
            .expect("pack pickaxe is grantable");
        assert_eq!(slot, 2);
        assert_eq!(item.count(), 1);
        assert_eq!(item.durability(), Some(900));
        assert_eq!(item.max_durability(), Some(900));
        assert!(item.is_tool());

        let (slot, item) = player
            .grant_palette_item_with(&registry, ItemId::Block(BlockId::Stone))
            .expect("built-in stone is grantable");
        assert_eq!(slot, 3);
        assert_eq!(item.count(), 999);
        assert_eq!(player.selected_block(), Some(BlockId::Stone));

        for empty in 4..HOTBAR_SLOTS {
            assert_eq!(
                player.inventory().hotbar(empty),
                None,
                "grant writes exactly one slot"
            );
        }
    }

    #[test]
    fn palette_grant_rejects_unknown_and_placeholder_without_mutation() {
        let mut registry = reference_registry();
        let placeholder_block = registry.append_placeholder_block("save:missing_block");
        let placeholder_item = registry.append_placeholder_item("save:missing_item");
        let mut player = creative_player();
        let before = player.inventory().clone();

        for id in [
            ItemId::Item(u16::MAX),
            ItemId::ModTool(u16::MAX),
            ItemId::Block(placeholder_block),
            ItemId::Item(placeholder_item),
            ItemId::Block(BlockId::Modded(0)),
        ] {
            assert_eq!(
                player.grant_palette_item_with(&registry, id),
                Err(ItemError::UnknownContent),
                "{id:?} must fail closed"
            );
            assert_eq!(player.inventory(), &before, "{id:?} mutated the inventory");
            assert_eq!(player.selected_slot(), 0, "{id:?} changed the selection");
        }
    }

    // -----------------------------------------------------------------------
    // M014/S01: config-driven movement rules
    // -----------------------------------------------------------------------

    #[test]
    fn default_movement_rules_equal_today() {
        let rules = MovementRules::default();
        assert_eq!(rules.walk_speed, WALK_SPEED);
        assert_eq!(rules.sprint_speed, SPRINT_SPEED);
        assert_eq!(rules.fly_speed, FLY_SPEED);
        assert_eq!(rules.fly_sprint_speed, FLY_SPRINT_SPEED);
        assert_eq!(rules.fly_vertical_factor, FLY_VERTICAL_FACTOR);
        assert_eq!(rules.jump_velocity, JUMP_VELOCITY);
        assert_eq!(rules.gravity, GRAVITY);
        assert_eq!(rules.terminal_velocity, TERMINAL_VELOCITY);
        assert_eq!(rules.swim_vertical_drag, SWIM_VERTICAL_DRAG);
        assert_eq!(rules.swim_rise_accel, SWIM_RISE_ACCEL);
        assert_eq!(rules.swim_rise_speed, SWIM_RISE_SPEED);
        assert_eq!(rules.swim_max_sink_speed, SWIM_MAX_SINK_SPEED);
        assert_eq!(rules.swim_walk_speed_mult, SWIM_WALK_SPEED_MULT);
        assert_eq!(rules.swim_sink_accel, SWIM_SINK_ACCEL);
    }

    #[test]
    fn non_default_walk_speed_changes_displacement() {
        fn one_second_displacement(movement: MovementRules) -> f32 {
            let mut player = Player::new(Vec3::new(0.5, 100.0, 0.5));
            player.fly_mode = false;
            player.movement = movement;
            player.yaw = 0.0;
            let forward = InputState {
                forward: true,
                ..Default::default()
            };
            let start = player.position;
            for _ in 0..60 {
                player.update(1.0 / 60.0, &forward, |_, _, _| BlockId::Air);
            }
            Vec3::new(
                player.position.x - start.x,
                0.0,
                player.position.z - start.z,
            )
            .length()
        }

        let default_distance = one_second_displacement(MovementRules::default());
        assert!(
            (default_distance - WALK_SPEED).abs() < 0.2,
            "default walk speed must move {WALK_SPEED} blocks in 1 s, got {default_distance}"
        );

        let fast = MovementRules {
            walk_speed: 9.0,
            ..MovementRules::default()
        };
        let fast_distance = one_second_displacement(fast);
        assert!(
            (fast_distance - 9.0).abs() < 0.2,
            "walk_speed = 9.0 must move 9 blocks in 1 s, got {fast_distance}"
        );
    }
}
