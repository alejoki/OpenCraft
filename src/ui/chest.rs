//! Pure 27+36 chest screen: state, pixel-space layout, hit testing, quads,
//! and labels.
//!
//! Same purity contract as `src/ui/inventory.rs`: no winit, wgpu, `Player`,
//! filesystem, or clock. The only outside values are data-only chest and
//! inventory domain state plus `glam::IVec3` (the live chest station supplied
//! by `WorldSession`), so the two-grid geometry is provable headless before
//! any runtime input can move items.
//!
//! Coordinates are physical pixels, top-left origin, y down — the same space
//! as `Quad`, `ctx.config`, glyphon `TextArea`, and `GameApp.cursor_pos`
//! (P003/MEM012). `main.rs` owns the frame loop and rebuilds this layout every
//! frame, so a `Resized` reflows the screen automatically.

use glam::IVec3;

use crate::player::container::Container;
use crate::player::inventory::{
    Inventory, Item, CHEST_SLOTS, HOTBAR_SLOTS, INVENTORY_SLOTS, MAIN_SLOTS,
};
use crate::ui::inventory::{
    Rect, HOTBAR_GAP, HOTBAR_SEPARATOR_THICKNESS, LABEL_LINE_HEIGHT, PANEL_MARGIN, SEPARATOR_COLOR,
    SLOT_GAP, SLOT_SIZE,
};
use crate::ui::Quad;

// ---------------------------------------------------------------------------
// Natural layout metrics (scaled to fit the surface)
// ---------------------------------------------------------------------------

/// Hard cap on labels emitted for one chest frame.
///
/// Worst case: 1 title + 63 count labels + 63 tool durability labels + 1 held
/// summary + 1 hint + 1 status = 130. The cap leaves headroom without ever
/// becoming reachable by the screen.
pub const MAX_CHEST_LABELS: usize = 160;

const SLOT_COLUMNS: usize = 9;
const STORAGE_ROWS: usize = MAIN_SLOTS / SLOT_COLUMNS;
const GRID_W: f32 = 9.0 * SLOT_SIZE + 8.0 * SLOT_GAP;
const CHEST_GRID_H: f32 = 3.0 * SLOT_SIZE + 2.0 * SLOT_GAP;
const STORAGE_H: f32 = STORAGE_ROWS as f32 * SLOT_SIZE + (STORAGE_ROWS as f32 - 1.0) * SLOT_GAP;

const PANEL_PAD: f32 = 16.0;
const TITLE_HEIGHT: f32 = 24.0;
const TITLE_GAP: f32 = 8.0;
const SECTION_GAP: f32 = 12.0;
const FOOTER_HEIGHT: f32 = 52.0;

const SLOTS_TOP: f32 = PANEL_PAD + TITLE_HEIGHT + TITLE_GAP;
const INVENTORY_TOP: f32 = SLOTS_TOP + CHEST_GRID_H + SECTION_GAP;
const HOTBAR_TOP: f32 = INVENTORY_TOP + STORAGE_H + HOTBAR_GAP;
const HOTBAR_SEPARATOR_TOP: f32 =
    INVENTORY_TOP + STORAGE_H + (HOTBAR_GAP - HOTBAR_SEPARATOR_THICKNESS) * 0.5;
const PANEL_W: f32 = 2.0 * PANEL_PAD + GRID_W;
const PANEL_H: f32 = HOTBAR_TOP + SLOT_SIZE + FOOTER_HEIGHT + PANEL_PAD;

const PANEL_COLOR: [f32; 4] = [0.07, 0.07, 0.09, 0.94];
const SLOT_COLOR: [f32; 4] = [0.20, 0.20, 0.24, 1.0];

// ---------------------------------------------------------------------------
// Screen state
// ---------------------------------------------------------------------------

/// Open/attached state of the chest screen.
///
/// The screen deliberately does not remember *which* chest it belongs to: the
/// world session owns the open station, and [`ChestScreen::station_lost`]
/// derives staleness from that live value every frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChestScreen {
    open: bool,
}

impl ChestScreen {
    /// Closed chest screen.
    pub const fn new() -> Self {
        Self { open: false }
    }

    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the screen attached to the live chest station.
    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// True only when the screen is open but the chest station has vanished
    /// (broken, emptied, or out of reach).
    pub const fn station_lost(&self, open_chest: Option<IVec3>) -> bool {
        self.open && open_chest.is_none()
    }
}

// ---------------------------------------------------------------------------
// Rendering bundle
// ---------------------------------------------------------------------------

/// One resolved chest frame: the open screen, its pixel layout, and the two
/// item grids. Bundled so the pure quad/icon builders stay a function of one
/// frame snapshot instead of a long argument list.
#[derive(Clone, Copy)]
pub struct ChestPanel<'a> {
    pub screen: &'a ChestScreen,
    pub layout: &'a ChestLayout,
    pub chest: Option<&'a Container>,
    pub inventory: &'a Inventory,
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// All pixel-space geometry for one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChestLayout {
    /// Dark backdrop panel.
    pub panel: Rect,
    /// 27 chest slots in display order (three rows of nine).
    pub chest_slots: [Rect; CHEST_SLOTS],
    /// 36 player slots: `9..36` are the three main-storage rows at the top,
    /// and `0..9` are the hotbar's bottom row below them.
    pub inventory_slots: [Rect; INVENTORY_SLOTS],
    /// Divider line between the storage rows and the hotbar row, spanning the
    /// slot grid exactly like the player inventory screen.
    pub hotbar_separator: Rect,
}

/// Builds the layout for the given surface. Centers the panel and scales the
/// natural metrics down when the surface is smaller than the panel, keeping
/// every coordinate finite and every rect non-negative for 0x0 and 1x1
/// surfaces.
pub fn layout(surface_w: u32, surface_h: u32) -> ChestLayout {
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

    let chest_slots = std::array::from_fn(|index| {
        let col = index % SLOT_COLUMNS;
        let row = index / SLOT_COLUMNS;
        to_px(
            PANEL_PAD + col as f32 * (SLOT_SIZE + SLOT_GAP),
            SLOTS_TOP + row as f32 * (SLOT_SIZE + SLOT_GAP),
            SLOT_SIZE,
            SLOT_SIZE,
        )
    });

    let inventory_slots = std::array::from_fn(|index| {
        let col = index % SLOT_COLUMNS;
        let y = if index < HOTBAR_SLOTS {
            // Public hotbar indices stay 0..9; only their row moves to the
            // bottom of the slot block, matching the inventory screen.
            HOTBAR_TOP
        } else {
            let storage_index = index - HOTBAR_SLOTS;
            let row = storage_index / SLOT_COLUMNS;
            INVENTORY_TOP + row as f32 * (SLOT_SIZE + SLOT_GAP)
        };
        to_px(
            PANEL_PAD + col as f32 * (SLOT_SIZE + SLOT_GAP),
            y,
            SLOT_SIZE,
            SLOT_SIZE,
        )
    });

    ChestLayout {
        panel: to_px(0.0, 0.0, PANEL_W, PANEL_H),
        chest_slots,
        inventory_slots,
        hotbar_separator: to_px(
            PANEL_PAD,
            HOTBAR_SEPARATOR_TOP,
            GRID_W,
            HOTBAR_SEPARATOR_THICKNESS,
        ),
    }
}

// ---------------------------------------------------------------------------
// Hit testing
// ---------------------------------------------------------------------------

fn contains((x, y, w, h): Rect, (cx, cy): (f32, f32)) -> bool {
    cx >= x && cx < x + w && cy >= y && cy < y + h
}

/// One clickable chest-screen target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChestTarget {
    /// One of the 27 chest slots.
    ChestSlot(usize),
    /// One of the 36 player inventory slots.
    InventorySlot(usize),
}

/// Which target is under the cursor, or `None` for empty screen space.
///
/// Half-open containment (left/top inside, right/bottom outside) matches
/// `ui::inventory::hit_test`, so neighbouring cells never both match. A
/// closed screen never reports a target.
pub fn hit_test(
    screen: &ChestScreen,
    layout: &ChestLayout,
    cursor: (f32, f32),
) -> Option<ChestTarget> {
    if !screen.is_open() {
        return None;
    }
    if let Some(index) = layout
        .chest_slots
        .iter()
        .position(|rect| contains(*rect, cursor))
    {
        return Some(ChestTarget::ChestSlot(index));
    }
    if let Some(index) = layout
        .inventory_slots
        .iter()
        .position(|rect| contains(*rect, cursor))
    {
        return Some(ChestTarget::InventorySlot(index));
    }
    None
}

/// Rect of a target, or `None` for an out-of-range index.
pub fn target_rect(layout: &ChestLayout, target: ChestTarget) -> Option<Rect> {
    match target {
        ChestTarget::ChestSlot(index) => layout.chest_slots.get(index).copied(),
        ChestTarget::InventorySlot(index) => layout.inventory_slots.get(index).copied(),
    }
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

/// All quads for one frame: backdrop, chest and inventory slot outlines, the
/// storage/hotbar separator, and nothing else. Empty for a closed screen.
///
/// Item art is textured, not tinted: `ui::item_icon::chest_icon_vertices_with`
/// draws every occupied cell after these outlines. A missing chest record
/// (`None`) renders as twenty-seven empty slots; the caller decides whether
/// that means "not loaded yet" or "untouched".
pub fn screen_quads(screen: &ChestScreen, layout: &ChestLayout) -> Vec<Quad> {
    if !screen.is_open() {
        return Vec::new();
    }

    let mut quads = Vec::with_capacity(2 + CHEST_SLOTS + INVENTORY_SLOTS);
    quads.push(rect_quad(layout.panel, PANEL_COLOR));
    quads.push(rect_quad(layout.hotbar_separator, SEPARATOR_COLOR));

    for rect in layout.chest_slots.iter() {
        quads.push(rect_quad(*rect, SLOT_COLOR));
    }
    for rect in layout.inventory_slots.iter() {
        quads.push(rect_quad(*rect, SLOT_COLOR));
    }

    quads
}

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

/// One line of text with its pixel-space top-left origin.
#[derive(Clone, Debug, PartialEq)]
pub struct ChestLabel {
    pub text: String,
    pub left: f32,
    pub top: f32,
}

fn cell_inset(rect: Rect) -> f32 {
    4.0_f32.min(rect.2 * 0.25).max(0.0)
}

/// Bottom-left count origin inside a cell, mirroring the inventory screen's
/// layout without depending on its private helper.
fn count_origin(rect: Rect) -> (f32, f32) {
    let inset = cell_inset(rect);
    let top = (rect.1 + rect.3 - LABEL_LINE_HEIGHT - inset).max(rect.1 + inset);
    (rect.0 + inset, top)
}

fn push_cell_labels(labels: &mut Vec<ChestLabel>, rect: Rect, item: Item) {
    let (left, top) = count_origin(rect);
    labels.push(ChestLabel {
        text: item.count().to_string(),
        left,
        top,
    });
    if let (Some(remaining), Some(max)) = (item.durability(), item.max_durability()) {
        labels.push(ChestLabel {
            text: format!("{remaining}/{max}"),
            left: rect.0 + cell_inset(rect),
            top: rect.1 + cell_inset(rect),
        });
    }
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
/// Emits the title, one count per occupied chest and inventory slot (plus a
/// `n/m` durability line for tools), the held-item readout, a fixed hint
/// line, and the optional status line. Empty for a closed screen.
pub fn screen_labels(
    screen: &ChestScreen,
    layout: &ChestLayout,
    chest: Option<&Container>,
    inventory: &Inventory,
    status: Option<&str>,
) -> Vec<ChestLabel> {
    let mut labels = Vec::new();
    if !screen.is_open() {
        return labels;
    }

    let pad_x = layout.panel.2 * (PANEL_PAD / PANEL_W);
    let pad_y = layout.panel.3 * (PANEL_PAD / PANEL_H);
    labels.push(ChestLabel {
        text: "Chest".to_string(),
        left: layout.panel.0 + pad_x,
        top: layout.panel.1 + pad_y,
    });

    for (index, rect) in layout.chest_slots.iter().enumerate() {
        if let Some(item) = chest.and_then(|chest| chest.slot(index)) {
            push_cell_labels(&mut labels, *rect, item);
        }
    }
    for (index, rect) in layout.inventory_slots.iter().enumerate() {
        if let Some(item) = inventory.slot(index) {
            push_cell_labels(&mut labels, *rect, item);
        }
    }

    // Footer rows: held item readout, hint, status. All inside the panel.
    let footer_top =
        layout.panel.1 + (PANEL_H - PANEL_PAD - FOOTER_HEIGHT) / PANEL_H * layout.panel.3;
    let footer_x = layout.panel.0 + pad_x;
    if let Some(item) = inventory.cursor() {
        labels.push(ChestLabel {
            text: held_summary(item),
            left: footer_x,
            top: footer_top,
        });
    }
    labels.push(ChestLabel {
        text: "Left click moves a stack".to_string(),
        left: footer_x,
        top: footer_top + LABEL_LINE_HEIGHT,
    });
    if let Some(status) = status.filter(|text| !text.is_empty()) {
        labels.push(ChestLabel {
            text: status.to_string(),
            left: footer_x,
            top: footer_top + 2.0 * LABEL_LINE_HEIGHT,
        });
    }

    debug_assert!(
        labels.len() <= MAX_CHEST_LABELS,
        "chest screen emitted {} labels, exceeding MAX_CHEST_LABELS={}",
        labels.len(),
        MAX_CHEST_LABELS
    );
    labels
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::inventory::{ItemId, ToolKind, ToolTier};
    use crate::world::block::BlockId;

    fn item(id: ItemId, count: u16) -> Item {
        Item::new(id, count).expect("test items are valid")
    }

    fn stone(count: u16) -> Item {
        item(ItemId::Block(BlockId::Stone), count)
    }

    fn worn_pickaxe() -> Item {
        Item::tool_with_durability(ToolKind::Pickaxe, ToolTier::Iron, 249)
            .expect("worn iron pickaxe is valid")
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

    fn all_targets() -> Vec<ChestTarget> {
        let mut targets: Vec<_> = (0..CHEST_SLOTS).map(ChestTarget::ChestSlot).collect();
        targets.extend((0..INVENTORY_SLOTS).map(ChestTarget::InventorySlot));
        targets
    }

    fn open_screen() -> ChestScreen {
        let mut screen = ChestScreen::new();
        screen.open();
        screen
    }

    #[test]
    fn layout_has_exactly_27_chest_and_36_inventory_slots() {
        let layout = layout(1280, 720);
        assert_eq!(layout.chest_slots.len(), 27);
        assert_eq!(layout.inventory_slots.len(), 36);

        let all_rects: Vec<Rect> = layout
            .chest_slots
            .iter()
            .chain(layout.inventory_slots.iter())
            .copied()
            .collect();
        for (index, rect) in all_rects.iter().enumerate() {
            let (x, y, w, h) = *rect;
            assert!(
                x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite(),
                "slot {index} at {rect:?} is not finite"
            );
            assert!(w > 0.0 && h > 0.0, "slot {index} has no area: {rect:?}");
            assert!(
                inside_panel(*rect, layout.panel),
                "slot {index} at {rect:?} escapes panel {:?}",
                layout.panel
            );
        }

        for (index, rect) in all_rects.iter().enumerate() {
            for (other_index, other) in all_rects.iter().enumerate().skip(index + 1) {
                assert!(
                    !rects_overlap(*rect, *other),
                    "slots {index} and {other_index} overlap: {rect:?} vs {other:?}"
                );
            }
        }

        // Chest grid is three rows of nine above the player storage rows.
        for row in 0..3 {
            let first = row * SLOT_COLUMNS;
            let row_top = layout.chest_slots[first].1;
            for col in 0..SLOT_COLUMNS {
                assert_eq!(layout.chest_slots[first + col].1, row_top);
            }
        }
        // Player storage rows are indices 9..36; the hotbar (0..9) is the
        // bottom row, separated by the divider exactly like the inventory.
        let storage_top = layout.inventory_slots[HOTBAR_SLOTS].1;
        for row in 0..STORAGE_ROWS {
            let first = HOTBAR_SLOTS + row * SLOT_COLUMNS;
            let row_top = layout.inventory_slots[first].1;
            assert_eq!(row_top, storage_top + row as f32 * (SLOT_SIZE + SLOT_GAP));
            for col in 0..SLOT_COLUMNS {
                assert_eq!(layout.inventory_slots[first + col].1, row_top);
            }
        }
        let hotbar_top = layout.inventory_slots[0].1;
        for index in 0..HOTBAR_SLOTS {
            assert_eq!(layout.inventory_slots[index].1, hotbar_top);
            assert!(
                layout.inventory_slots[index].1
                    > layout.inventory_slots[HOTBAR_SLOTS + (STORAGE_ROWS - 1) * SLOT_COLUMNS].1,
                "hotbar slot {index} must sit below every storage row"
            );
        }

        let last_chest = layout.chest_slots[CHEST_SLOTS - 1];
        let storage_bottom = storage_top + STORAGE_H;
        assert!(
            last_chest.1 + last_chest.3 < storage_top,
            "chest grid must sit fully above the player grid"
        );

        // The divider sits inside HOTBAR_GAP, below storage and above the
        // hotbar, and overlaps no slot.
        let separator = layout.hotbar_separator;
        assert!(inside_panel(separator, layout.panel));
        assert!((separator.0 - layout.panel.0 - PANEL_PAD).abs() <= 1e-3);
        assert!((separator.2 - GRID_W).abs() <= 1e-3);
        assert!(
            separator.1 >= storage_bottom,
            "separator starts above storage"
        );
        assert!(
            separator.1 + separator.3 <= hotbar_top,
            "separator ends below the hotbar top"
        );
        for (index, rect) in layout
            .chest_slots
            .iter()
            .chain(layout.inventory_slots.iter())
            .enumerate()
        {
            assert!(
                !rects_overlap(separator, *rect),
                "slot {index} overlaps the separator"
            );
        }
    }

    #[test]
    fn half_open_hit_testing_covers_exactly_the_slots() {
        let layout = layout(1280, 720);
        let screen = open_screen();

        for target in all_targets() {
            let rect = target_rect(&layout, target).expect("target has a rect");
            assert_eq!(hit_test(&screen, &layout, center(rect)), Some(target));
            // Left/top edges are inside; right/bottom edges are outside.
            assert_eq!(hit_test(&screen, &layout, (rect.0, rect.1)), Some(target));
            assert_eq!(
                hit_test(&screen, &layout, (rect.0 + rect.2, center(rect).1)),
                None,
                "{target:?} right edge must be half-open"
            );
            assert_eq!(
                hit_test(&screen, &layout, (center(rect).0, rect.1 + rect.3)),
                None,
                "{target:?} bottom edge must be half-open"
            );
        }

        // Out-of-range targets have no rect.
        assert_eq!(target_rect(&layout, ChestTarget::ChestSlot(27)), None);
        assert_eq!(
            target_rect(&layout, ChestTarget::ChestSlot(usize::MAX)),
            None
        );
        assert_eq!(target_rect(&layout, ChestTarget::InventorySlot(36)), None);

        // The closed screen reports nothing, even on a live slot centre.
        let closed = ChestScreen::new();
        for target in all_targets() {
            let rect = target_rect(&layout, target).expect("target has a rect");
            assert_eq!(hit_test(&closed, &layout, center(rect)), None);
        }
        assert_eq!(hit_test(&screen, &layout, (-1.0, -1.0)), None);
        assert_eq!(hit_test(&screen, &layout, (f32::NAN, 0.0)), None);
        assert_eq!(hit_test(&screen, &layout, (0.0, f32::INFINITY)), None);
    }

    #[test]
    fn closed_screen_emits_no_quads_or_labels() {
        let layout = layout(1280, 720);
        let closed = ChestScreen::new();
        let chest = Container::from_slots(std::array::from_fn(|_| Some(stone(3))));
        let inventory =
            Inventory::from_parts(std::array::from_fn(|_| Some(stone(2))), Some(stone(5)));

        assert!(screen_quads(&closed, &layout).is_empty());
        assert!(
            screen_labels(&closed, &layout, Some(&chest), &inventory, Some("hidden")).is_empty()
        );
    }

    #[test]
    fn screen_quads_emit_only_the_panel_separator_and_slot_outlines() {
        let layout = layout(1280, 720);
        let screen = open_screen();

        let quads = screen_quads(&screen, &layout);
        // Panel + separator + 27 chest and 36 inventory outlines.
        assert_eq!(quads.len(), 2 + CHEST_SLOTS + INVENTORY_SLOTS);
        assert_eq!(quads[0].color, PANEL_COLOR);
        assert_eq!(quads[1].color, SEPARATOR_COLOR);
        for quad in &quads[2..] {
            assert_eq!(quad.color, SLOT_COLOR, "slots draw only their outline");
        }
    }

    #[test]
    fn missing_chest_renders_empty_slots() {
        let layout = layout(1280, 720);
        let screen = open_screen();
        let inventory = Inventory::empty();

        // Quads are content-independent: item art is a separate textured pass.
        assert_eq!(
            screen_quads(&screen, &layout).len(),
            2 + CHEST_SLOTS + INVENTORY_SLOTS
        );

        let labels = screen_labels(&screen, &layout, None, &inventory, None);
        assert_eq!(labels.len(), 2, "title plus hint only");
        assert_eq!(labels[0].text, "Chest");
        assert!(!labels.iter().any(|label| label.text == "0"));
    }

    #[test]
    fn degenerate_surfaces_stay_finite_and_safe() {
        for (w, h) in [(0_u32, 0_u32), (1, 1), (100, 80)] {
            let layout = layout(w, h);
            let rects = std::iter::once(layout.panel)
                .chain(std::iter::once(layout.hotbar_separator))
                .chain(layout.chest_slots)
                .chain(layout.inventory_slots);
            for rect in rects {
                assert!(
                    rect.0.is_finite()
                        && rect.1.is_finite()
                        && rect.2.is_finite()
                        && rect.3.is_finite(),
                    "{w}x{h} produced a non-finite rect {rect:?}"
                );
                assert!(rect.2 >= 0.0 && rect.3 >= 0.0);
                assert!(
                    inside_panel(rect, layout.panel),
                    "{w}x{h} rect {rect:?} escapes panel {:?}",
                    layout.panel
                );
            }

            let screen = open_screen();
            let chest = Container::from_slots(std::array::from_fn(|_| Some(stone(1))));
            let inventory =
                Inventory::from_parts(std::array::from_fn(|_| Some(stone(1))), Some(stone(1)));
            for cursor in [(0.0, 0.0), (w as f32 * 0.5, h as f32 * 0.5), (-5.0, -5.0)] {
                let _ = hit_test(&screen, &layout, cursor);
            }
            let _ = screen_quads(&screen, &layout);
            let labels = screen_labels(&screen, &layout, Some(&chest), &inventory, Some("status"));
            for label in labels {
                assert!(label.left.is_finite() && label.top.is_finite());
            }
        }
    }

    #[test]
    fn labels_cover_all_slots_and_stay_bounded() {
        const {
            assert!(
                MAX_CHEST_LABELS >= 130,
                "documented worst case must fit the cap"
            )
        };

        let layout = layout(1280, 720);
        let screen = open_screen();
        let chest = Container::from_slots(std::array::from_fn(|_| Some(worn_pickaxe())));
        let inventory = Inventory::from_parts(
            std::array::from_fn(|_| Some(worn_pickaxe())),
            Some(worn_pickaxe()),
        );
        let status = "Chest opened";

        let labels = screen_labels(&screen, &layout, Some(&chest), &inventory, Some(status));
        assert!(labels.len() <= MAX_CHEST_LABELS, "{} labels", labels.len());
        // 1 title + 63 counts + 63 durability lines + 1 held + 1 hint + 1 status.
        assert_eq!(labels.len(), 130);

        let texts: Vec<&str> = labels.iter().map(|label| label.text.as_str()).collect();
        assert!(texts.contains(&"Chest"));
        assert_eq!(
            texts.iter().filter(|text| **text == "1").count(),
            63,
            "one count label per filled slot"
        );
        assert_eq!(
            texts.iter().filter(|text| **text == "249/250").count(),
            63,
            "one durability label per filled tool slot"
        );
        assert!(texts.contains(&"Holding: Iron Pickaxe x1 (249/250)"));
        assert!(texts.contains(&"Left click moves a stack"));
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
    }

    #[test]
    fn ten_x_surface_keeps_draw_lists_bounded() {
        let layout_1x = layout(1280, 720);
        let layout_10x = layout(12_800, 7_200);
        let chest = Container::from_slots(std::array::from_fn(|_| Some(worn_pickaxe())));
        let inventory = Inventory::from_parts(
            std::array::from_fn(|_| Some(worn_pickaxe())),
            Some(worn_pickaxe()),
        );
        let screen = open_screen();

        for layout in [&layout_1x, &layout_10x] {
            let quads = screen_quads(&screen, layout);
            // 1 panel + 1 separator + 63 outlines.
            assert_eq!(quads.len(), 65);
            let labels = screen_labels(&screen, layout, Some(&chest), &inventory, Some("status"));
            assert_eq!(labels.len(), 130);
            assert!(labels.len() <= MAX_CHEST_LABELS);
        }
    }

    #[test]
    fn screen_state_reports_station_loss_only_while_open() {
        let mut screen = ChestScreen::new();
        assert_eq!(screen, ChestScreen::default());
        assert!(!screen.is_open());
        assert!(!screen.station_lost(None));
        assert!(!screen.station_lost(Some(IVec3::ZERO)));

        screen.open();
        assert!(screen.is_open());
        assert!(screen.station_lost(None));
        assert!(!screen.station_lost(Some(IVec3::new(4, 70, -9))));

        screen.close();
        assert!(!screen.is_open());
        assert!(!screen.station_lost(None));
    }
}
