//! Pure inventory/crafting screen: mode state, pixel-space layout, hit
//! testing, quads, and labels.
//!
//! Same purity contract as `src/ui/screens.rs`: no winit, wgpu, `Player`,
//! filesystem, or clock. The only outside types are data-only inventory
//! domain values and `glam::IVec3` (the live crafting-table position supplied
//! by `WorldSession`). Everything is a pure function of its arguments, so the
//! full 36-slot + 4x4 geometry is provable headless before any runtime input
//! can move items.
//!
//! Coordinates are physical pixels, top-left origin, y down — the same space
//! as `Quad`, `ctx.config`, glyphon `TextArea`, and `GameApp.cursor_pos`
//! (P003/MEM012). `main.rs` (later slices) owns the frame loop and rebuilds
//! this layout every frame, so a `Resized` reflows the screen automatically.

use glam::IVec3;

use crate::player::inventory::{
    CraftingGrid, GridKind, Inventory, Item, ItemId, RecipePreview, ToolKind, ToolTier,
    INVENTORY_SLOTS, MAX_GRID_SLOTS, PLAYER_GRID_DIM, TABLE_GRID_DIM,
};
use crate::ui::Quad;
use crate::world::block::BlockId;

// ---------------------------------------------------------------------------
// Natural layout metrics (scaled to fit the surface)
// ---------------------------------------------------------------------------

/// Natural slot edge length in px before surface fitting.
pub const SLOT_SIZE: f32 = 40.0;
/// Natural gap between neighbouring cells in px.
pub const SLOT_GAP: f32 = 4.0;
/// Empty border kept between the panel and the surface edge, in px.
pub const PANEL_MARGIN: f32 = 8.0;
/// Line height used to anchor label rows, in px.
pub const LABEL_LINE_HEIGHT: f32 = 16.0;
/// Hard cap on labels emitted for one frame.
///
/// Worst case: 1 title + 36 slot counts + 36 tool durability lines + 4 player
/// cells + 16 table cells + 2 results + 1 cursor + 1 hint + 1 status = 98.
/// The cap leaves headroom without ever becoming reachable by the screen.
pub const MAX_LABELS: usize = 128;

const PANEL_PAD: f32 = 16.0;
const TITLE_HEIGHT: f32 = 24.0;
const TITLE_GAP: f32 = 8.0;
const COLUMN_GAP: f32 = 24.0;
const SECTION_GAP: f32 = 12.0;
const RESULT_GAP: f32 = 16.0;
const FOOTER_HEIGHT: f32 = 52.0;

/// Hotbar slots form row 0 of the 9x4 slot grid.
const SLOT_COLUMNS: usize = 9;
const SLOTS_W: f32 = 9.0 * SLOT_SIZE + 8.0 * SLOT_GAP;
const PLAYER_GRID_W: f32 = 2.0 * SLOT_SIZE + SLOT_GAP;
const PLAYER_GRID_H: f32 = PLAYER_GRID_W;
const TABLE_GRID_W: f32 = 4.0 * SLOT_SIZE + 3.0 * SLOT_GAP;
const TABLE_GRID_H: f32 = TABLE_GRID_W;
const CRAFT_X: f32 = PANEL_PAD + SLOTS_W + COLUMN_GAP;
const CRAFT_W: f32 = TABLE_GRID_W + RESULT_GAP + SLOT_SIZE;
const CRAFT_H: f32 = PLAYER_GRID_H + SECTION_GAP + TABLE_GRID_H;
const SLOTS_TOP: f32 = PANEL_PAD + TITLE_HEIGHT + TITLE_GAP;
const TABLE_TOP: f32 = SLOTS_TOP + PLAYER_GRID_H + SECTION_GAP;
const PANEL_W: f32 = 2.0 * PANEL_PAD + SLOTS_W + COLUMN_GAP + CRAFT_W;
const PANEL_H: f32 = SLOTS_TOP + CRAFT_H + FOOTER_HEIGHT + PANEL_PAD;

const PANEL_COLOR: [f32; 4] = [0.07, 0.07, 0.09, 0.94];
const SLOT_COLOR: [f32; 4] = [0.20, 0.20, 0.24, 1.0];
const GRID_COLOR: [f32; 4] = [0.17, 0.18, 0.21, 1.0];
const RESULT_COLOR: [f32; 4] = [0.28, 0.25, 0.14, 1.0];

/// Fraction of a slot edge used as the inset for the item fill quad.
const FILL_INSET_RATIO: f32 = 0.12;
/// Alpha of the cursor ghost quad (the identity color itself stays opaque).
const CURSOR_GHOST_ALPHA: f32 = 0.85;

// ---------------------------------------------------------------------------
// Mode + screen state
// ---------------------------------------------------------------------------

/// Which inventory surface is on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InventoryMode {
    /// No inventory screen.
    Closed,
    /// Player inventory with the 2x2 grid.
    Player,
    /// Crafting table with the 4x4 grid.
    Table,
}

impl InventoryMode {
    pub const fn is_open(self) -> bool {
        !matches!(self, InventoryMode::Closed)
    }
}

/// Pixel-space rect: `(x, y, w, h)`, top-left origin, y down.
pub type Rect = (f32, f32, f32, f32);

/// One clickable inventory target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InventoryTarget {
    /// One of the 36 inventory slots (`0..9` hotbar, `9..36` main storage).
    Slot(usize),
    /// One active crafting cell in the given grid.
    GridCell { kind: GridKind, index: usize },
    /// The result slot of the given grid.
    ResultSlot(GridKind),
}

/// Open/attached state of the inventory screen.
///
/// The screen merely remembers that it was opened *for* a table; whether the
/// table is still live is decided every frame from `WorldSession`'s
/// `table_open`, which keeps a stale screen from granting 4x4 access.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InventoryScreen {
    open: bool,
    table_attached: bool,
}

impl InventoryScreen {
    pub const fn new() -> Self {
        Self {
            open: false,
            table_attached: false,
        }
    }

    pub const fn is_open(&self) -> bool {
        self.open
    }

    pub const fn table_attached(&self) -> bool {
        self.table_attached
    }

    /// Opens the player inventory (E key path).
    pub fn open_player(&mut self) {
        self.open = true;
        self.table_attached = false;
    }

    /// Opens the screen attached to a crafting-table session.
    pub fn open_table(&mut self) {
        self.open = true;
        self.table_attached = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.table_attached = false;
    }

    /// Opens the closed screen (attached when a table is live) or closes an
    /// already-open one. One key toggles both paths.
    pub fn toggle(&mut self, table_open: Option<IVec3>) {
        if self.open {
            self.close();
        } else {
            self.open = true;
            self.table_attached = table_open.is_some();
        }
    }

    /// Effective mode for this frame. A closed screen is `Closed` even when a
    /// table is live; `Table` requires an attached screen *and* a live table.
    pub const fn mode(&self, table_open: Option<IVec3>) -> InventoryMode {
        if !self.open {
            InventoryMode::Closed
        } else if self.table_attached && table_open.is_some() {
            InventoryMode::Table
        } else {
            InventoryMode::Player
        }
    }

    /// True only when the screen was opened for a table that has since
    /// disappeared (broken or out of range).
    pub const fn table_lost(&self, table_open: Option<IVec3>) -> bool {
        self.open && self.table_attached && table_open.is_none()
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// All pixel-space geometry for one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InventoryLayout {
    /// Dark backdrop panel.
    pub panel: Rect,
    /// 36 slots: `0..9` hotbar row, `9..36` three main-storage rows.
    pub slots: [Rect; INVENTORY_SLOTS],
    /// Active 2x2 player cells, row-major.
    pub player_grid: [Rect; PLAYER_GRID_DIM * PLAYER_GRID_DIM],
    /// All 16 table cells, row-major. Populated in every mode; only
    /// hit-tested and drawn in [`InventoryMode::Table`].
    pub table_grid: [Rect; MAX_GRID_SLOTS],
    /// Player 2x2 result slot.
    pub player_result: Rect,
    /// Table 4x4 result slot.
    pub table_result: Rect,
}

/// Builds the layout for the given surface. Centers the panel and scales the
/// natural metrics down when the surface is smaller than the panel, keeping
/// every coordinate finite and every rect non-negative for 0x0 and 1x1
/// surfaces.
///
/// Geometry is identical in every mode (the table column is always measured);
/// `mode` is accepted so callers cannot forget to pass it, and so future mode
/// chrome can be added without an API break.
pub fn layout(surface_w: u32, surface_h: u32, _mode: InventoryMode) -> InventoryLayout {
    let surface_w = surface_w.max(1) as f32;
    let surface_h = surface_h.max(1) as f32;
    let avail_w = (surface_w - 2.0 * PANEL_MARGIN).max(1.0);
    let avail_h = (surface_h - 2.0 * PANEL_MARGIN).max(1.0);
    // Both ratios are strictly positive (avail_* >= 1, PANEL_* > 1), so the
    // scale can never be zero, negative, or NaN.
    let scale = (avail_w / PANEL_W).min(avail_h / PANEL_H).min(1.0);
    let panel_w = PANEL_W * scale;
    let panel_h = PANEL_H * scale;
    let origin_x = (surface_w - panel_w) * 0.5;
    let origin_y = (surface_h - panel_h) * 0.5;

    let to_px = |x: f32, y: f32, w: f32, h: f32| -> Rect {
        (
            origin_x + x * scale,
            origin_y + y * scale,
            w * scale,
            h * scale,
        )
    };

    let slots = std::array::from_fn(|index| {
        let col = index % SLOT_COLUMNS;
        let row = index / SLOT_COLUMNS;
        to_px(
            PANEL_PAD + col as f32 * (SLOT_SIZE + SLOT_GAP),
            SLOTS_TOP + row as f32 * (SLOT_SIZE + SLOT_GAP),
            SLOT_SIZE,
            SLOT_SIZE,
        )
    });

    let player_grid = std::array::from_fn(|index| {
        let col = index % PLAYER_GRID_DIM;
        let row = index / PLAYER_GRID_DIM;
        to_px(
            CRAFT_X + col as f32 * (SLOT_SIZE + SLOT_GAP),
            SLOTS_TOP + row as f32 * (SLOT_SIZE + SLOT_GAP),
            SLOT_SIZE,
            SLOT_SIZE,
        )
    });

    let table_grid = std::array::from_fn(|index| {
        let col = index % TABLE_GRID_DIM;
        let row = index / TABLE_GRID_DIM;
        to_px(
            CRAFT_X + col as f32 * (SLOT_SIZE + SLOT_GAP),
            TABLE_TOP + row as f32 * (SLOT_SIZE + SLOT_GAP),
            SLOT_SIZE,
            SLOT_SIZE,
        )
    });

    InventoryLayout {
        panel: to_px(0.0, 0.0, PANEL_W, PANEL_H),
        slots,
        player_grid,
        table_grid,
        player_result: to_px(
            CRAFT_X + PLAYER_GRID_W + RESULT_GAP,
            SLOTS_TOP + (PLAYER_GRID_H - SLOT_SIZE) * 0.5,
            SLOT_SIZE,
            SLOT_SIZE,
        ),
        table_result: to_px(
            CRAFT_X + TABLE_GRID_W + RESULT_GAP,
            TABLE_TOP + (TABLE_GRID_H - SLOT_SIZE) * 0.5,
            SLOT_SIZE,
            SLOT_SIZE,
        ),
    }
}

// ---------------------------------------------------------------------------
// Hit testing
// ---------------------------------------------------------------------------

fn contains((x, y, w, h): Rect, (cx, cy): (f32, f32)) -> bool {
    cx >= x && cx < x + w && cy >= y && cy < y + h
}

/// Which target is under the cursor, or `None` for empty screen space.
///
/// Half-open containment (left/top inside, right/bottom outside) matches
/// `ui::menu::hit_test`, so neighbouring cells never both match. `Closed`
/// never reports a target; `Table` cells are invisible to `Player` mode.
pub fn hit_test(
    mode: InventoryMode,
    layout: &InventoryLayout,
    cursor: (f32, f32),
) -> Option<InventoryTarget> {
    if !mode.is_open() {
        return None;
    }
    if let Some(index) = layout.slots.iter().position(|rect| contains(*rect, cursor)) {
        return Some(InventoryTarget::Slot(index));
    }
    if let Some(index) = layout
        .player_grid
        .iter()
        .position(|rect| contains(*rect, cursor))
    {
        return Some(InventoryTarget::GridCell {
            kind: GridKind::Player,
            index,
        });
    }
    if contains(layout.player_result, cursor) {
        return Some(InventoryTarget::ResultSlot(GridKind::Player));
    }
    if mode == InventoryMode::Table {
        if let Some(index) = layout
            .table_grid
            .iter()
            .position(|rect| contains(*rect, cursor))
        {
            return Some(InventoryTarget::GridCell {
                kind: GridKind::Table,
                index,
            });
        }
        if contains(layout.table_result, cursor) {
            return Some(InventoryTarget::ResultSlot(GridKind::Table));
        }
    }
    None
}

/// Rect of a target in the given mode, or `None` when the target is not
/// reachable in that mode (table targets in `Player`/`Closed` mode).
pub fn target_rect(
    mode: InventoryMode,
    layout: &InventoryLayout,
    target: InventoryTarget,
) -> Option<Rect> {
    if !mode.is_open() {
        return None;
    }
    match target {
        InventoryTarget::Slot(index) => layout.slots.get(index).copied(),
        InventoryTarget::GridCell { kind, index } => match kind {
            GridKind::Player => layout.player_grid.get(index).copied(),
            GridKind::Table if mode == InventoryMode::Table => {
                layout.table_grid.get(index).copied()
            }
            GridKind::Table => None,
        },
        InventoryTarget::ResultSlot(GridKind::Player) => Some(layout.player_result),
        InventoryTarget::ResultSlot(GridKind::Table) if mode == InventoryMode::Table => {
            Some(layout.table_result)
        }
        InventoryTarget::ResultSlot(GridKind::Table) => None,
    }
}

// ---------------------------------------------------------------------------
// Item palette
// ---------------------------------------------------------------------------

/// Deterministic opaque palette per item identity. No art files: the UI can
/// draw recognizable stacks from pure data.
///
/// Air, bedrock, and water are uncarryable, so their shared fallback color can
/// never appear as an item.
pub fn item_color(item: Item) -> [f32; 4] {
    match item.id() {
        ItemId::Block(block) => block_color(block),
        ItemId::Material(_) => [0.60, 0.42, 0.24, 1.0],
        ItemId::Tool { kind, tier } => tool_color(kind, tier),
    }
}

fn block_color(block: BlockId) -> [f32; 4] {
    match block {
        BlockId::Grass => [0.30, 0.62, 0.26, 1.0],
        BlockId::Dirt => [0.45, 0.32, 0.20, 1.0],
        BlockId::Stone => [0.50, 0.50, 0.52, 1.0],
        BlockId::Cobblestone => [0.42, 0.42, 0.44, 1.0],
        BlockId::Sand => [0.85, 0.80, 0.55, 1.0],
        BlockId::OakLog => [0.40, 0.28, 0.15, 1.0],
        BlockId::OakLeaves => [0.18, 0.45, 0.18, 1.0],
        BlockId::OakPlanks => [0.66, 0.50, 0.28, 1.0],
        BlockId::Brick => [0.62, 0.26, 0.20, 1.0],
        BlockId::Snow => [0.92, 0.94, 0.97, 1.0],
        BlockId::CoalOre => [0.28, 0.28, 0.30, 1.0],
        BlockId::IronOre => [0.60, 0.52, 0.45, 1.0],
        BlockId::CraftingTable => [0.58, 0.34, 0.18, 1.0],
        // Water is uncarryable, so this palette entry is unreachable through
        // the item UI; it exists so the block color map stays total and a
        // future HUD/debug readout has a canonical tint.
        BlockId::Water => [0.20, 0.42, 0.78, 1.0],
        BlockId::Air | BlockId::Bedrock => [0.16, 0.16, 0.18, 1.0],
    }
}

fn tool_color(kind: ToolKind, tier: ToolTier) -> [f32; 4] {
    let base = match tier {
        ToolTier::Wood => [0.55, 0.38, 0.22],
        ToolTier::Stone => [0.55, 0.55, 0.57],
        ToolTier::Iron => [0.76, 0.79, 0.83],
    };
    let (dr, dg, db) = match kind {
        ToolKind::Pickaxe => (0.0, 0.0, 0.0),
        ToolKind::Axe => (0.07, -0.05, -0.07),
        ToolKind::Shovel => (-0.05, 0.05, -0.02),
    };
    [base[0] + dr, base[1] + dg, base[2] + db, 1.0]
}

// ---------------------------------------------------------------------------
// Quads
// ---------------------------------------------------------------------------

fn rect_quad(rect: Rect, color: [f32; 4]) -> Quad {
    Quad {
        x: rect.0,
        y: rect.1,
        w: rect.2,
        h: rect.3,
        color,
    }
}

/// Shrinks a rect by `amount` on every edge without ever inverting it.
fn inset((x, y, w, h): Rect, amount: f32) -> Rect {
    let dx = amount.min(w * 0.5).max(0.0);
    let dy = amount.min(h * 0.5).max(0.0);
    (
        x + dx,
        y + dy,
        (w - 2.0 * dx).max(0.0),
        (h - 2.0 * dy).max(0.0),
    )
}

/// Fill rect for one occupied cell: the identity color inset inside the slot
/// outline so the outline stays visible.
fn cell_fill(rect: Rect) -> Rect {
    inset(rect, rect.2 * FILL_INSET_RATIO)
}

fn preview_fill(rect: Rect, preview: RecipePreview) -> Option<Quad> {
    let item = Item::new(preview.output, preview.output_count).ok()?;
    Some(rect_quad(cell_fill(rect), item_color(item)))
}

/// All quads for one frame: backdrop, slot/grid outlines, item fills, result
/// previews, and the cursor ghost. Empty for a closed screen.
pub fn screen_quads(
    mode: InventoryMode,
    layout: &InventoryLayout,
    inventory: &Inventory,
    player_grid: &CraftingGrid,
    table_grid: &CraftingGrid,
    cursor_item: Option<Item>,
    cursor_pos: Option<(f32, f32)>,
) -> Vec<Quad> {
    if !mode.is_open() {
        return Vec::new();
    }
    let mut quads = Vec::new();
    quads.push(rect_quad(layout.panel, PANEL_COLOR));

    for (index, rect) in layout.slots.iter().enumerate() {
        quads.push(rect_quad(*rect, SLOT_COLOR));
        if let Some(item) = inventory.slot(index) {
            quads.push(rect_quad(cell_fill(*rect), item_color(item)));
        }
    }

    for (index, rect) in layout.player_grid.iter().enumerate() {
        quads.push(rect_quad(*rect, GRID_COLOR));
        if let Some(item) = player_grid.slot(index) {
            quads.push(rect_quad(cell_fill(*rect), item_color(item)));
        }
    }

    quads.push(rect_quad(layout.player_result, RESULT_COLOR));
    if let Some(preview) = player_grid.preview() {
        quads.extend(preview_fill(layout.player_result, preview));
    }

    if mode == InventoryMode::Table {
        for (index, rect) in layout.table_grid.iter().enumerate() {
            quads.push(rect_quad(*rect, GRID_COLOR));
            if let Some(item) = table_grid.slot(index) {
                quads.push(rect_quad(cell_fill(*rect), item_color(item)));
            }
        }
        quads.push(rect_quad(layout.table_result, RESULT_COLOR));
        if let Some(preview) = table_grid.preview() {
            quads.extend(preview_fill(layout.table_result, preview));
        }
    }

    if let (Some(item), Some((cx, cy))) = (cursor_item, cursor_pos) {
        if cx.is_finite() && cy.is_finite() {
            let size = layout.player_result.2 * 0.75;
            let color = item_color(item);
            quads.push(Quad {
                x: cx - size * 0.5,
                y: cy - size * 0.5,
                w: size,
                h: size,
                color: [color[0], color[1], color[2], CURSOR_GHOST_ALPHA],
            });
        }
    }

    quads
}

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

/// One line of text with its pixel-space top-left origin.
#[derive(Clone, Debug, PartialEq)]
pub struct InventoryLabel {
    pub text: String,
    pub left: f32,
    pub top: f32,
}

fn cell_inset(rect: Rect) -> f32 {
    4.0_f32.min(rect.2 * 0.25).max(0.0)
}

/// Bottom-left count origin inside a cell, mirroring
/// `ui::hud::hotbar_count_text_origin` without depending on the render stack.
fn count_origin(rect: Rect) -> (f32, f32) {
    let inset = cell_inset(rect);
    let top = (rect.1 + rect.3 - LABEL_LINE_HEIGHT - inset).max(rect.1 + inset);
    (rect.0 + inset, top)
}

fn push_cell_labels(labels: &mut Vec<InventoryLabel>, rect: Rect, item: Item) {
    let (left, top) = count_origin(rect);
    labels.push(InventoryLabel {
        text: item.count().to_string(),
        left,
        top,
    });
    if let (Some(remaining), Some(max)) = (item.durability(), item.max_durability()) {
        labels.push(InventoryLabel {
            text: format!("{remaining}/{max}"),
            left: rect.0 + cell_inset(rect),
            top: rect.1 + cell_inset(rect),
        });
    }
}

fn push_result_label(labels: &mut Vec<InventoryLabel>, rect: Rect, preview: RecipePreview) {
    let (left, top) = count_origin(rect);
    labels.push(InventoryLabel {
        text: format!("{} x{}", preview.output.name(), preview.output_count),
        left,
        top,
    });
}

fn held_summary(item: Item) -> String {
    let mut text = format!("Holding: {} x{}", item.id().name(), item.count());
    if let (Some(remaining), Some(max)) = (item.durability(), item.max_durability()) {
        text.push_str(&format!(" ({remaining}/{max})"));
    }
    text
}

/// All labels for one frame, positioned inside their owning rect.
///
/// Emits the title, one count per occupied inventory slot (plus a `n/m`
/// durability line for tools), one count per occupied grid cell, result
/// previews, the held-item readout, a fixed hint line, and the optional
/// status line. Empty for a closed screen, and no per-slot labels for an
/// empty inventory.
pub fn screen_labels(
    mode: InventoryMode,
    layout: &InventoryLayout,
    inventory: &Inventory,
    player_grid: &CraftingGrid,
    table_grid: &CraftingGrid,
    status: Option<&str>,
) -> Vec<InventoryLabel> {
    let mut labels = Vec::new();
    if !mode.is_open() {
        return labels;
    }

    let pad_x = layout.panel.2 * (PANEL_PAD / PANEL_W);
    let pad_y = layout.panel.3 * (PANEL_PAD / PANEL_H);
    let title = match mode {
        InventoryMode::Table => "Crafting Table",
        _ => "Player Inventory",
    };
    labels.push(InventoryLabel {
        text: title.to_string(),
        left: layout.panel.0 + pad_x,
        top: layout.panel.1 + pad_y,
    });

    for (index, rect) in layout.slots.iter().enumerate() {
        if let Some(item) = inventory.slot(index) {
            push_cell_labels(&mut labels, *rect, item);
        }
    }
    for (index, rect) in layout.player_grid.iter().enumerate() {
        if let Some(item) = player_grid.slot(index) {
            push_cell_labels(&mut labels, *rect, item);
        }
    }
    if mode == InventoryMode::Table {
        for (index, rect) in layout.table_grid.iter().enumerate() {
            if let Some(item) = table_grid.slot(index) {
                push_cell_labels(&mut labels, *rect, item);
            }
        }
    }

    if let Some(preview) = player_grid.preview() {
        push_result_label(&mut labels, layout.player_result, preview);
    }
    if mode == InventoryMode::Table {
        if let Some(preview) = table_grid.preview() {
            push_result_label(&mut labels, layout.table_result, preview);
        }
    }

    // Footer rows: held item readout, hint, status. All inside the panel.
    let footer_top =
        layout.panel.1 + (PANEL_H - PANEL_PAD - FOOTER_HEIGHT) / PANEL_H * layout.panel.3;
    let footer_x = layout.panel.0 + pad_x;
    if let Some(item) = inventory.cursor() {
        labels.push(InventoryLabel {
            text: held_summary(item),
            left: footer_x,
            top: footer_top,
        });
    }
    labels.push(InventoryLabel {
        text: "Left click moves a stack, right click splits or places one".to_string(),
        left: footer_x,
        top: footer_top + LABEL_LINE_HEIGHT,
    });
    if let Some(status) = status.filter(|text| !text.is_empty()) {
        labels.push(InventoryLabel {
            text: status.to_string(),
            left: footer_x,
            top: footer_top + 2.0 * LABEL_LINE_HEIGHT,
        });
    }

    labels
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::inventory::{Material, HOTBAR_SLOTS, MAIN_SLOTS};

    fn item(id: ItemId, count: u16) -> Item {
        Item::new(id, count).expect("test items are valid")
    }

    fn stone(count: u16) -> Item {
        item(ItemId::Block(BlockId::Stone), count)
    }

    fn center((x, y, w, h): Rect) -> (f32, f32) {
        (x + w * 0.5, y + h * 0.5)
    }

    fn inside_panel(rect: Rect, panel: Rect) -> bool {
        let (x, y, w, h) = rect;
        let (px, py, pw, ph) = panel;
        w >= 0.0
            && h >= 0.0
            && x >= px
            && y >= py
            && x + w <= px + pw + f32::EPSILON
            && y + h <= py + ph + f32::EPSILON
    }

    fn rects_overlap(a: Rect, b: Rect) -> bool {
        let (ax, ay, aw, ah) = a;
        let (bx, by, bw, bh) = b;
        ax < bx + bw && bx < ax + aw && ay < by + bh && by < ay + ah
    }

    fn all_targets(mode: InventoryMode) -> Vec<InventoryTarget> {
        let mut targets: Vec<_> = (0..INVENTORY_SLOTS).map(InventoryTarget::Slot).collect();
        targets.extend((0..PLAYER_GRID_DIM * PLAYER_GRID_DIM).map(|index| {
            InventoryTarget::GridCell {
                kind: GridKind::Player,
                index,
            }
        }));
        targets.push(InventoryTarget::ResultSlot(GridKind::Player));
        if mode == InventoryMode::Table {
            targets.extend((0..MAX_GRID_SLOTS).map(|index| InventoryTarget::GridCell {
                kind: GridKind::Table,
                index,
            }));
            targets.push(InventoryTarget::ResultSlot(GridKind::Table));
        }
        targets
    }

    fn filled_inventory() -> Inventory {
        let slots = std::array::from_fn(|index| {
            Some(if index == 0 {
                Item::tool_with_durability(ToolKind::Pickaxe, ToolTier::Iron, 249)
                    .expect("worn iron pickaxe is valid")
            } else {
                stone(index as u16 + 1)
            })
        });
        Inventory::from_parts(slots, Some(stone(5)))
    }

    fn full_grid(kind: GridKind, id: ItemId) -> CraftingGrid {
        let mut grid = CraftingGrid::new(kind);
        for index in 0..kind.slot_count() {
            grid.set(index, Some(item(id, 1))).expect("cell in range");
        }
        grid
    }

    fn iron_pickaxe_grid() -> CraftingGrid {
        let mut grid = CraftingGrid::new(GridKind::Table);
        for index in [0, 1, 2, 5, 9] {
            let id = if index <= 2 {
                ItemId::Block(BlockId::IronOre)
            } else {
                ItemId::Material(Material::Stick)
            };
            grid.set(index, Some(item(id, 1))).expect("cell in range");
        }
        grid
    }

    #[test]
    fn layout_has_exactly_36_distinct_targets_with_hotbar_first() {
        let layout = layout(1280, 720, InventoryMode::Player);
        let panel = layout.panel;

        for (index, rect) in layout.slots.iter().enumerate() {
            assert!(
                inside_panel(*rect, panel),
                "slot {index} at {rect:?} escapes panel {panel:?}"
            );
            for (other_index, other) in layout.slots.iter().enumerate().skip(index + 1) {
                assert!(
                    !rects_overlap(*rect, *other),
                    "slots {index} and {other_index} overlap: {rect:?} vs {other:?}"
                );
            }
        }
        assert_eq!(layout.slots.len(), HOTBAR_SLOTS + MAIN_SLOTS);

        // Row 0 is the hotbar: same top, increasing left, uniform size.
        let hotbar_top = layout.slots[0].1;
        let mut last_left = f32::NEG_INFINITY;
        for index in 0..HOTBAR_SLOTS {
            let rect = layout.slots[index];
            assert_eq!(rect.1, hotbar_top, "hotbar slot {index} top");
            assert!(rect.0 > last_left, "hotbar slot {index} left is increasing");
            last_left = rect.0;
        }

        // Rows 1..4 are the main storage: three distinct rows of nine.
        let mut row_tops = [f32::NAN; 3];
        for (row, row_top) in row_tops.iter_mut().enumerate() {
            let first = HOTBAR_SLOTS + row * HOTBAR_SLOTS;
            *row_top = layout.slots[first].1;
            for col in 0..HOTBAR_SLOTS {
                let rect = layout.slots[first + col];
                assert_eq!(
                    rect.1, *row_top,
                    "main row {row} col {col} shares the row top"
                );
            }
        }
        assert!(row_tops[0] > hotbar_top);
        assert!(row_tops[1] > row_tops[0]);
        assert!(row_tops[2] > row_tops[1]);
    }

    #[test]
    fn player_mode_hit_tests_slots_grid_and_result_and_ignores_table() {
        let layout = layout(1280, 720, InventoryMode::Player);

        for index in 0..INVENTORY_SLOTS {
            assert_eq!(
                hit_test(InventoryMode::Player, &layout, center(layout.slots[index])),
                Some(InventoryTarget::Slot(index))
            );
        }
        for index in 0..PLAYER_GRID_DIM * PLAYER_GRID_DIM {
            assert_eq!(
                hit_test(
                    InventoryMode::Player,
                    &layout,
                    center(layout.player_grid[index])
                ),
                Some(InventoryTarget::GridCell {
                    kind: GridKind::Player,
                    index
                })
            );
        }
        assert_eq!(
            hit_test(InventoryMode::Player, &layout, center(layout.player_result)),
            Some(InventoryTarget::ResultSlot(GridKind::Player))
        );

        // Table rects exist in the layout but are unreachable in Player mode.
        for rect in layout.table_grid {
            assert_eq!(hit_test(InventoryMode::Player, &layout, center(rect)), None);
        }
        assert_eq!(
            hit_test(InventoryMode::Player, &layout, center(layout.table_result)),
            None
        );
        assert_eq!(hit_test(InventoryMode::Player, &layout, (-1.0, -1.0)), None);

        // No table quads are emitted in Player mode.
        let inventory = Inventory::empty();
        let player_grid = CraftingGrid::new(GridKind::Player);
        let table_grid = CraftingGrid::new(GridKind::Table);
        let quads = screen_quads(
            InventoryMode::Player,
            &layout,
            &inventory,
            &player_grid,
            &table_grid,
            None,
            None,
        );
        // panel + 36 slots + 4 player cells + player result
        assert_eq!(quads.len(), 42);
    }

    #[test]
    fn table_mode_activates_all_sixteen_cells_and_result() {
        let layout = layout(1280, 720, InventoryMode::Table);

        for index in 0..MAX_GRID_SLOTS {
            assert_eq!(
                hit_test(
                    InventoryMode::Table,
                    &layout,
                    center(layout.table_grid[index])
                ),
                Some(InventoryTarget::GridCell {
                    kind: GridKind::Table,
                    index
                })
            );
        }
        assert_eq!(
            hit_test(InventoryMode::Table, &layout, center(layout.table_result)),
            Some(InventoryTarget::ResultSlot(GridKind::Table))
        );
        // Player-mode targets stay reachable from the table screen.
        for index in 0..INVENTORY_SLOTS {
            assert_eq!(
                hit_test(InventoryMode::Table, &layout, center(layout.slots[index])),
                Some(InventoryTarget::Slot(index))
            );
        }
        for index in 0..PLAYER_GRID_DIM * PLAYER_GRID_DIM {
            assert_eq!(
                hit_test(
                    InventoryMode::Table,
                    &layout,
                    center(layout.player_grid[index])
                ),
                Some(InventoryTarget::GridCell {
                    kind: GridKind::Player,
                    index
                })
            );
        }

        let quads = screen_quads(
            InventoryMode::Table,
            &layout,
            &Inventory::empty(),
            &CraftingGrid::new(GridKind::Player),
            &CraftingGrid::new(GridKind::Table),
            None,
            None,
        );
        // 42 player-mode quads + 16 table cells + table result
        assert_eq!(quads.len(), 42 + 16 + 1);
    }

    #[test]
    fn closed_mode_has_no_targets_or_quads() {
        let layout = layout(1280, 720, InventoryMode::Closed);
        let inventory = filled_inventory();
        let player_grid = full_grid(GridKind::Player, ItemId::Block(BlockId::OakPlanks));
        let table_grid = full_grid(GridKind::Table, ItemId::Block(BlockId::OakPlanks));

        for rect in layout.slots {
            assert_eq!(hit_test(InventoryMode::Closed, &layout, center(rect)), None);
        }
        for rect in layout.player_grid {
            assert_eq!(hit_test(InventoryMode::Closed, &layout, center(rect)), None);
        }
        assert_eq!(
            hit_test(InventoryMode::Closed, &layout, center(layout.player_result)),
            None
        );
        assert_eq!(
            hit_test(InventoryMode::Closed, &layout, center(layout.table_result)),
            None
        );
        assert!(screen_quads(
            InventoryMode::Closed,
            &layout,
            &inventory,
            &player_grid,
            &table_grid,
            Some(stone(1)),
            Some((10.0, 10.0)),
        )
        .is_empty());
        assert!(screen_labels(
            InventoryMode::Closed,
            &layout,
            &inventory,
            &player_grid,
            &table_grid,
            Some("hidden"),
        )
        .is_empty());
    }

    #[test]
    fn targets_do_not_overlap_in_either_open_mode() {
        for mode in [InventoryMode::Player, InventoryMode::Table] {
            let layout = layout(1280, 720, mode);
            let targets = all_targets(mode);
            for (index, target) in targets.iter().enumerate() {
                let rect = target_rect(mode, &layout, *target)
                    .unwrap_or_else(|| panic!("{mode:?} target {target:?} has a rect"));
                for other in targets.iter().skip(index + 1) {
                    let other_rect = target_rect(mode, &layout, *other)
                        .expect("every listed target is reachable in this mode");
                    assert!(
                        !rects_overlap(rect, other_rect),
                        "{mode:?}: {target:?} {rect:?} overlaps {other:?} {other_rect:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn degenerate_surfaces_stay_finite_and_safe() {
        for (w, h) in [(0_u32, 0_u32), (1, 1), (100, 80)] {
            for mode in [
                InventoryMode::Closed,
                InventoryMode::Player,
                InventoryMode::Table,
            ] {
                let layout = layout(w, h, mode);
                let rects = std::iter::once(layout.panel)
                    .chain(layout.slots)
                    .chain(layout.player_grid)
                    .chain(layout.table_grid)
                    .chain([layout.player_result, layout.table_result]);
                for rect in rects {
                    assert!(
                        rect.0.is_finite()
                            && rect.1.is_finite()
                            && rect.2.is_finite()
                            && rect.3.is_finite(),
                        "{w}x{h} {mode:?} produced a non-finite rect {rect:?}"
                    );
                    assert!(rect.2 >= 0.0 && rect.3 >= 0.0);
                }
                // Hit testing and quad building must not panic at any size.
                for cursor in [(0.0, 0.0), (w as f32 * 0.5, h as f32 * 0.5), (-5.0, -5.0)] {
                    let _ = hit_test(mode, &layout, cursor);
                }
                let inventory = Inventory::empty();
                let player_grid = CraftingGrid::new(GridKind::Player);
                let table_grid = CraftingGrid::new(GridKind::Table);
                let _ = screen_quads(
                    mode,
                    &layout,
                    &inventory,
                    &player_grid,
                    &table_grid,
                    Some(stone(1)),
                    Some((0.0, 0.0)),
                );
                let _ = screen_labels(
                    mode,
                    &layout,
                    &inventory,
                    &player_grid,
                    &table_grid,
                    Some("status"),
                );
            }
        }
    }

    #[test]
    fn mode_derivation_and_table_loss() {
        let table = Some(IVec3::new(4, 70, -9));
        let mut screen = InventoryScreen::new();
        assert_eq!(screen, InventoryScreen::default());
        assert!(!screen.is_open());
        assert_eq!(screen.mode(table), InventoryMode::Closed);
        assert!(!screen.table_lost(table));
        assert!(!screen.table_lost(None));

        // Player inventory ignores a live table.
        screen.open_player();
        assert!(screen.is_open());
        assert!(!screen.table_attached());
        assert_eq!(screen.mode(table), InventoryMode::Player);
        assert_eq!(screen.mode(None), InventoryMode::Player);
        assert!(!screen.table_lost(None));

        // Table screen without a live table degrades to Player and reports loss.
        screen.open_table();
        assert!(screen.table_attached());
        assert_eq!(screen.mode(table), InventoryMode::Table);
        assert!(!screen.table_lost(table));
        assert_eq!(screen.mode(None), InventoryMode::Player);
        assert!(screen.table_lost(None));

        // Closing wins over a live table.
        screen.close();
        assert_eq!(screen.mode(table), InventoryMode::Closed);
        assert!(!screen.table_lost(table));
        assert!(!screen.table_attached());

        // Toggle opens attached only when a table is live.
        screen.toggle(None);
        assert!(screen.is_open());
        assert_eq!(screen.mode(None), InventoryMode::Player);
        assert!(!screen.table_attached());
        screen.toggle(None);
        assert!(!screen.is_open());
        screen.toggle(Some(IVec3::ZERO));
        assert_eq!(screen.mode(Some(IVec3::ZERO)), InventoryMode::Table);
        assert!(screen.table_attached());

        let mut screen = InventoryScreen::new();
        screen.toggle(table);
        assert_eq!(screen.mode(table), InventoryMode::Table);
        screen.toggle(table);
        assert!(!screen.is_open());
        assert_eq!(screen.mode(table), InventoryMode::Closed);
    }

    #[test]
    fn labels_cover_counts_durability_result_cursor_status_and_stay_bounded() {
        const { assert!(MAX_LABELS >= 62, "documented worst case must fit the cap") };

        let layout = layout(1280, 720, InventoryMode::Table);
        let inventory = filled_inventory();
        let player_grid = full_grid(GridKind::Player, ItemId::Block(BlockId::OakPlanks));
        let table_grid = full_grid(GridKind::Table, ItemId::Block(BlockId::OakPlanks));
        let status = "Table opened";

        // Full table filled with planks matches no recipe, so only the player
        // result label is emitted.
        let labels = screen_labels(
            InventoryMode::Table,
            &layout,
            &inventory,
            &player_grid,
            &table_grid,
            Some(status),
        );
        assert!(labels.len() <= MAX_LABELS, "{} labels", labels.len());
        // 1 title + 36 slot counts + 1 durability + 4 player cells + 16 table
        // cells + 1 player result + 1 held + 1 hint + 1 status.
        assert_eq!(labels.len(), 62);

        let texts: Vec<&str> = labels.iter().map(|label| label.text.as_str()).collect();
        assert!(texts.contains(&"Crafting Table"));
        assert!(texts.contains(&"249/250"), "worn tool durability shown");
        assert!(
            texts.contains(&"Crafting Table x1"),
            "player result preview"
        );
        assert!(texts.contains(&"Holding: Stone x5"));
        assert!(texts.contains(&"Left click moves a stack, right click splits or places one"));
        assert!(texts.contains(&status));

        for label in &labels {
            assert!(label.left.is_finite() && label.top.is_finite());
            let panel = layout.panel;
            assert!(
                label.left >= panel.0
                    && label.top >= panel.1
                    && label.left <= panel.0 + panel.2
                    && label.top + LABEL_LINE_HEIGHT <= panel.1 + panel.3,
                "label {:?} escapes the panel",
                label.text
            );
        }

        // A real table recipe yields the table result label too.
        let table_preview = iron_pickaxe_grid();
        assert!(table_preview.preview().is_some());
        let labels = screen_labels(
            InventoryMode::Table,
            &layout,
            &inventory,
            &CraftingGrid::new(GridKind::Player),
            &table_preview,
            None,
        );
        assert!(labels.iter().any(|label| label.text == "Iron Pickaxe x1"));
        assert!(labels.len() <= MAX_LABELS);

        // An empty inventory emits no per-slot count labels.
        let empty = Inventory::empty();
        let empty_labels = screen_labels(
            InventoryMode::Player,
            &layout,
            &empty,
            &CraftingGrid::new(GridKind::Player),
            &CraftingGrid::new(GridKind::Table),
            None,
        );
        assert_eq!(empty_labels.len(), 2, "title plus hint only");
        assert_eq!(empty_labels[0].text, "Player Inventory");
        assert!(!empty_labels.iter().any(|label| label.text == "0"));
    }

    #[test]
    fn item_colors_are_opaque_and_distinct_for_different_identities() {
        let blocks = [
            BlockId::Grass,
            BlockId::Dirt,
            BlockId::Stone,
            BlockId::Cobblestone,
            BlockId::Sand,
            BlockId::OakLog,
            BlockId::OakLeaves,
            BlockId::OakPlanks,
            BlockId::Brick,
            BlockId::Snow,
            BlockId::CoalOre,
            BlockId::IronOre,
            BlockId::CraftingTable,
        ];
        let mut identities: Vec<ItemId> = blocks.into_iter().map(ItemId::Block).collect();
        identities.push(ItemId::Material(Material::Stick));
        for tier in [ToolTier::Wood, ToolTier::Stone, ToolTier::Iron] {
            for kind in [ToolKind::Pickaxe, ToolKind::Axe, ToolKind::Shovel] {
                identities.push(ItemId::Tool { kind, tier });
            }
        }

        let colors: Vec<[f32; 4]> = identities
            .iter()
            .map(|id| item_color(item(*id, 1)))
            .collect();
        for (identity, color) in identities.iter().zip(colors.iter()) {
            assert_eq!(color[3], 1.0, "{identity:?} must be opaque");
            for channel in &color[..3] {
                assert!(
                    (0.0..=1.0).contains(channel),
                    "{identity:?} channel {channel} out of range"
                );
            }
        }
        for (index, color) in colors.iter().enumerate() {
            for (other_index, other) in colors.iter().enumerate().skip(index + 1) {
                assert_ne!(
                    color, other,
                    "{:?} and {:?} share a palette entry",
                    identities[index], identities[other_index]
                );
            }
        }

        // Same identity always maps to the same color.
        let a = item(ItemId::Block(BlockId::Stone), 3);
        let b = item(ItemId::Block(BlockId::Stone), 64);
        assert_eq!(item_color(a), item_color(b));
    }

    #[test]
    fn water_block_color_is_defined_distinct_from_the_fallback() {
        // Water is uncarryable, so `item_color` can never reach it, but the
        // block palette stays total. It must not silently alias the shared
        // Air/Bedrock fallback.
        let water = block_color(BlockId::Water);
        let fallback = block_color(BlockId::Air);
        assert_eq!(water[3], 1.0, "water color must be opaque");
        assert_ne!(water, fallback, "water needs its own tint");
        assert_eq!(block_color(BlockId::Bedrock), fallback);
        for channel in &water[..3] {
            assert!(
                (0.0..=1.0).contains(channel),
                "water channel {channel} out of range"
            );
        }
    }
}
