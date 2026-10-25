//! Pure pixel-space HUD geometry.
//!
//! No GPU or window types appear here: callers feed surface dimensions and
//! append the returned [`Quad`]s to the existing UI quad list.

use crate::player::inventory::{Inventory, Item, ItemStack, HOTBAR_SLOTS};
use crate::render::pipeline::hotbar_slot_rect_ndc;
use crate::ui::quad::Quad;

/// Width of the crosshair mining progress bar in physical pixels.
pub const PROGRESS_BAR_WIDTH: f32 = 120.0;
/// Height of the crosshair mining progress bar in physical pixels.
pub const PROGRESS_BAR_HEIGHT: f32 = 8.0;
/// Vertical offset of the bar's top edge below the surface center.
pub const CROSSHAIR_BAR_OFFSET_Y: f32 = 24.0;

/// Font size for per-slot hotbar count labels, in physical pixels.
pub const HOTBAR_COUNT_FONT_SIZE: f32 = 14.0;
/// Line height for per-slot hotbar count labels, in physical pixels.
pub const HOTBAR_COUNT_LINE_HEIGHT: f32 = 16.0;

/// Hotbar slot rects in physical pixels `(x, y, w, h)` (top-left origin).
///
/// Derived from the same NDC helper the overlay outlines use, so labels and
/// outlines cannot drift. `max(1)` keeps the conversion finite on a
/// zero-sized surface, matching the rest of the HUD geometry.
pub fn hotbar_slot_rects(surface_w: u32, surface_h: u32) -> [(f32, f32, f32, f32); HOTBAR_SLOTS] {
    let w_px = surface_w.max(1) as f32;
    let h_px = surface_h.max(1) as f32;
    let aspect = w_px / h_px;

    std::array::from_fn(|index| {
        let (x0, y0, x1, y1) = hotbar_slot_rect_ndc(index, aspect);
        (
            (x0 + 1.0) / 2.0 * w_px,
            (1.0 - y1) / 2.0 * h_px,
            (x1 - x0) / 2.0 * w_px,
            (y1 - y0) / 2.0 * h_px,
        )
    })
}

/// Count label for a slot: `""` when empty, otherwise the decimal count.
/// Slot identity comes from the selected-stack title and mining order.
pub fn hotbar_count_label(stack: Option<ItemStack>) -> String {
    match stack {
        Some(stack) => stack.count.to_string(),
        None => String::new(),
    }
}

/// Count label for a hotbar slot, read straight off the durable inventory.
///
/// Takes a shared borrow so the HUD can observe counts but can never mutate
/// slot state; non-block hotbar items (tools) show their count of one.
pub fn hotbar_count_label_for(inventory: &Inventory, slot: usize) -> String {
    match inventory.hotbar(slot) {
        Some(item) => item.count().to_string(),
        None => String::new(),
    }
}

/// Selected-item title fragment for the window title: blocks and materials show
/// `Name xCount`, tools show `Name (remaining/max)`, and empty shows `Empty`.
pub fn selected_item_label(item: Option<Item>) -> String {
    let Some(item) = item else {
        return "Empty".to_string();
    };
    match (item.durability(), item.max_durability()) {
        (Some(remaining), Some(max)) => format!("{} ({}/{})", item.id().name(), remaining, max),
        _ => format!("{} x{}", item.id().name(), item.count()),
    }
}

/// Bottom-left origin for a slot's count label in physical pixels, inset
/// from the slot's left and bottom edges.
pub fn hotbar_count_text_origin(rect: (f32, f32, f32, f32)) -> (f32, f32) {
    let (x, y, _w, h) = rect;
    (x + 4.0, y + h - HOTBAR_COUNT_LINE_HEIGHT - 2.0)
}

/// Build the two quads that draw the mining progress bar: an always-visible
/// dark background followed by the amber fill scaled to `progress`.
///
/// Returns `[background, fill]` so the fill is drawn on top.
pub fn mining_progress_quads(progress: f32, surface_w: u32, surface_h: u32) -> [Quad; 2] {
    let progress = if progress.is_nan() {
        0.0
    } else {
        progress.clamp(0.0, 1.0)
    };

    // `max(1)` keeps the centering math finite on a zero-sized surface; on
    // narrower-than-bar surfaces the negative x clips symmetrically.
    let x = (surface_w.max(1) as f32 - PROGRESS_BAR_WIDTH) / 2.0;
    let y = surface_h.max(1) as f32 / 2.0 + CROSSHAIR_BAR_OFFSET_Y;

    let background = Quad {
        x,
        y,
        w: PROGRESS_BAR_WIDTH,
        h: PROGRESS_BAR_HEIGHT,
        color: [0.0, 0.0, 0.0, 0.55],
    };
    let fill = Quad {
        x,
        y,
        w: PROGRESS_BAR_WIDTH * progress,
        h: PROGRESS_BAR_HEIGHT,
        color: [0.95, 0.85, 0.25, 0.95],
    };

    [background, fill]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::block::BlockId;

    fn assert_close(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-4,
            "{what}: {actual} != {expected}"
        );
    }

    #[test]
    fn background_is_centered_below_crosshair() {
        let [bg, _] = mining_progress_quads(0.0, 1280, 720);
        assert_close(bg.x, 580.0, "background x");
        assert_close(bg.y, 384.0, "background y");
        assert_close(bg.w, PROGRESS_BAR_WIDTH, "background w");
        assert_close(bg.h, PROGRESS_BAR_HEIGHT, "background h");
    }

    #[test]
    fn fill_width_is_proportional_and_aligned_with_background() {
        for (progress, expected_w) in [(0.5, 60.0), (0.25, 30.0)] {
            let [bg, fill] = mining_progress_quads(progress, 1280, 720);
            assert_close(fill.w, expected_w, "fill w");
            assert_close(fill.x, bg.x, "fill x");
            assert_close(fill.y, bg.y, "fill y");
            assert_close(fill.h, bg.h, "fill h");
        }
    }

    #[test]
    fn progress_is_clamped_to_unit_range() {
        let [_, full] = mining_progress_quads(1.5, 1280, 720);
        assert_close(full.w, PROGRESS_BAR_WIDTH, "clamped high");

        let [_, negative] = mining_progress_quads(-1.0, 1280, 720);
        assert_close(negative.w, 0.0, "clamped low");

        let [_, nan] = mining_progress_quads(f32::NAN, 1280, 720);
        assert_close(nan.w, 0.0, "nan clamped to zero");
    }

    #[test]
    fn degenerate_surfaces_stay_finite() {
        for (w, h) in [(0, 0), (80, 60)] {
            let quads = mining_progress_quads(0.5, w, h);
            for quad in quads {
                assert!(quad.x.is_finite(), "x finite on {w}x{h}");
                assert!(quad.y.is_finite(), "y finite on {w}x{h}");
                assert!(quad.w.is_finite(), "w finite on {w}x{h}");
                assert!(quad.h.is_finite(), "h finite on {w}x{h}");
            }
        }

        // Narrow surface: symmetric clipping rather than a panic.
        let [bg, _] = mining_progress_quads(0.5, 80, 60);
        assert_close(bg.x, -20.0, "narrow background x");
        assert!(bg.x.is_finite());
    }

    #[test]
    fn background_and_fill_colors_differ_with_valid_alpha() {
        let [bg, fill] = mining_progress_quads(0.5, 1280, 720);
        assert_ne!(bg.color, fill.color, "colors differ");
        for (name, color) in [("background", bg.color), ("fill", fill.color)] {
            for channel in color {
                assert!(
                    (0.0..=1.0).contains(&channel),
                    "{name} channel {channel} out of range"
                );
            }
        }
    }

    #[test]
    fn hotbar_pixel_rects_match_1280x720_expectation() {
        let rects = hotbar_slot_rects(1280, 720);

        // Independently computed from the pixel-space constants:
        // slot width 32.4 px, gap 4.32 px, total bar 326.16 px, x centred at 640.
        let (x0, y0, w0, h0) = rects[0];
        assert_close(x0, 476.92, "slot 0 x");
        assert_close(y0, 660.6, "slot 0 y");
        assert_close(w0, 32.4, "slot 0 w");
        assert_close(h0, 30.6, "slot 0 h");

        let (x8, y8, w8, h8) = rects[HOTBAR_SLOTS - 1];
        assert_close(x8, 770.68, "slot 8 x");
        assert_close(y8, 660.6, "slot 8 y");
        assert_close(w8, 32.4, "slot 8 w");
        assert_close(h8, 30.6, "slot 8 h");

        // Centered: equal space to the left of slot 0 and right of slot 8.
        assert_close(x0, 1280.0 - (x8 + w8), "bar centered");
    }

    #[test]
    fn hotbar_pixel_rects_are_uniform_and_ordered() {
        let rects = hotbar_slot_rects(1280, 720);
        let (_, y0, w0, h0) = rects[0];

        for (i, rect) in rects.iter().enumerate() {
            assert_close(rect.1, y0, &format!("slot {i} y"));
            assert_close(rect.2, w0, &format!("slot {i} w"));
            assert_close(rect.3, h0, &format!("slot {i} h"));
            if i > 0 {
                assert!(
                    rect.0 > rects[i - 1].0,
                    "slot {i} x must increase with index"
                );
                assert!(rect.0 >= rects[i - 1].0 + rects[i - 1].2, "no overlap");
            }
        }
    }

    #[test]
    fn hotbar_pixel_rects_stay_finite_on_degenerate_surfaces() {
        for (w, h) in [(0, 0), (80, 60)] {
            for (i, rect) in hotbar_slot_rects(w, h).iter().enumerate() {
                for (name, value) in [("x", rect.0), ("y", rect.1), ("w", rect.2), ("h", rect.3)] {
                    assert!(value.is_finite(), "slot {i} {name} finite on {w}x{h}");
                }
            }
        }
    }

    #[test]
    fn count_labels_are_counts_only() {
        assert!(hotbar_count_label(None).is_empty());
        assert_eq!(hotbar_count_label(ItemStack::new(BlockId::CoalOre, 1)), "1");
        assert_eq!(
            hotbar_count_label(ItemStack::new(BlockId::CoalOre, 999)),
            "999"
        );
    }

    #[test]
    fn inventory_count_labels_read_the_hotbar_only() {
        use crate::player::inventory::{Item, ItemId, Material, INVENTORY_SLOTS};

        let mut inventory = Inventory::empty();
        inventory
            .collect_block(BlockId::CoalOre, 4)
            .expect("collect into the hotbar");
        assert_eq!(hotbar_count_label_for(&inventory, 0), "4");
        for slot in 1..HOTBAR_SLOTS {
            assert!(hotbar_count_label_for(&inventory, slot).is_empty());
        }

        // Main-storage and out-of-range indices never surface as hotbar labels.
        let mut slots = [None; INVENTORY_SLOTS];
        slots[HOTBAR_SLOTS] =
            Some(Item::new(ItemId::Material(Material::Stick), 12).expect("sticks"));
        let inventory = Inventory::from_parts(slots, None);
        for slot in 0..HOTBAR_SLOTS {
            assert!(hotbar_count_label_for(&inventory, slot).is_empty());
        }
        assert!(hotbar_count_label_for(&inventory, HOTBAR_SLOTS).is_empty());
        assert!(hotbar_count_label_for(&inventory, usize::MAX).is_empty());
    }

    #[test]
    fn selected_item_label_names_blocks_tools_and_empty() {
        use crate::player::inventory::{Item, ItemId, Material, ToolKind, ToolTier};

        assert_eq!(selected_item_label(None), "Empty");
        let stone = Item::new(ItemId::Block(BlockId::Stone), 3).expect("stone");
        assert_eq!(selected_item_label(Some(stone)), "Stone x3");
        let sticks = Item::new(ItemId::Material(Material::Stick), 1).expect("sticks");
        assert_eq!(selected_item_label(Some(sticks)), "Stick x1");
        let pickaxe =
            Item::tool_with_durability(ToolKind::Pickaxe, ToolTier::Iron, 137).expect("tool");
        assert_eq!(selected_item_label(Some(pickaxe)), "Iron Pickaxe (137/250)");
    }

    #[test]
    fn count_text_origin_is_inside_the_slot() {
        let rect = hotbar_slot_rects(1280, 720)[0];
        let (tx, ty) = hotbar_count_text_origin(rect);
        assert!(tx > rect.0 && tx < rect.0 + rect.2, "origin within slot x");
        assert!(
            ty >= rect.1 && ty < rect.1 + rect.3,
            "origin above the slot bottom and below its top"
        );
    }
}
