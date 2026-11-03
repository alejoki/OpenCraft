//! Pure pixel-space HUD geometry.
//!
//! No GPU or window types appear here: callers feed surface dimensions and
//! append the returned [`Quad`]s to the existing UI quad list.

use crate::player::inventory::{Inventory, Item, ItemStack, HOTBAR_SLOTS};
use crate::player::survival::{GameMode, SurvivalState, FULL_AIR, MAX_HEALTH, MAX_HUNGER};
use crate::render::pipeline::hotbar_slot_rect_ndc;
use crate::ui::inventory::Rect;
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

/// Base pixel size of one vitals icon (heart, hunger pip, or air bubble).
///
/// This is the unscaled reference size; [`vitals_metrics`] multiplies it by a
/// continuous scale derived from the measured hotbar width.
pub const VITALS_ICON_SIZE: f32 = 16.0;
/// Base horizontal gap between adjacent icons in one vitals row, in pixels.
pub const VITALS_ICON_GAP: f32 = 2.0;
/// Base vertical gap between the vitals rows, in pixels.
pub const VITALS_ROW_GAP: f32 = 4.0;
/// Dark background behind every vitals icon, so empty ones stay legible.
pub const VITALS_BACKGROUND_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 0.55];
/// Fill color of a heart icon.
pub const HEART_FILL_COLOR: [f32; 4] = [0.85, 0.15, 0.15, 0.95];
/// Fill color of a hunger pip.
pub const HUNGER_FILL_COLOR: [f32; 4] = [0.95, 0.65, 0.15, 0.95];
/// Fill color of an air bubble.
pub const AIR_FILL_COLOR: [f32; 4] = [0.25, 0.6, 0.95, 0.9];

/// Peak alpha of the full-screen red damage flash.
///
/// Deliberately translucent: the flash tints the whole frame (HUD included)
/// without hiding what the player is looking at, and it rides the existing
/// alpha-blended quad pass, so no new pipeline or texture is needed.
pub const DAMAGE_FLASH_ALPHA: f32 = 0.35;

/// Icons per vitals row: ten two-point hearts / hunger pips.
const VITALS_ICON_COUNT: usize = (MAX_HEALTH / 2) as usize;
/// Full width of one ten-icon vitals row at base scale, including its gaps.
pub const VITALS_ROW_WIDTH_BASE: f32 =
    VITALS_ICON_COUNT as f32 * VITALS_ICON_SIZE + (VITALS_ICON_COUNT - 1) as f32 * VITALS_ICON_GAP;
/// Total width of both vitals rows plus their center gap at base scale.
pub const VITALS_TOTAL_WIDTH_BASE: f32 = 2.0 * VITALS_ROW_WIDTH_BASE + VITALS_ICON_GAP;

/// Pixel geometry of the Survival vitals HUD for one surface.
///
/// Every field is the base constant scaled by the same continuous ratio, so a
/// single hotbar measurement keeps the whole stack proportional.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VitalsMetrics {
    /// Width and height of one icon, in physical pixels.
    pub icon_size: f32,
    /// Horizontal gap between adjacent icons in one row, in physical pixels.
    pub icon_gap: f32,
    /// Vertical gap between the heart/hunger row and the air row, in pixels.
    pub row_gap: f32,
    /// Width of one ten-icon row, in physical pixels.
    pub row_width: f32,
}

/// Vitals geometry scaled continuously from the measured hotbar width.
///
/// The hotbar is the single geometry source for the whole HUD. At base scale
/// one ten-icon row is [`VITALS_ROW_WIDTH_BASE`] px and the two rows plus their
/// center gap span [`VITALS_TOTAL_WIDTH_BASE`] px; scaling by
/// `hotbar_w / VITALS_TOTAL_WIDTH_BASE` keeps hearts and hunger inside the
/// hotbar and separated at every resolution, without breakpoints.
///
/// A non-finite or non-positive measured width falls back to the unscaled base
/// constants so degenerate surfaces still produce finite geometry.
pub fn vitals_metrics(surface_w: u32, surface_h: u32) -> VitalsMetrics {
    let rects = hotbar_slot_rects(surface_w, surface_h);
    let hotbar_w = (rects[HOTBAR_SLOTS - 1].0 + rects[HOTBAR_SLOTS - 1].2) - rects[0].0;
    vitals_metrics_from_hotbar_width(hotbar_w)
}

/// Scale the base vitals constants by one ratio derived from `hotbar_w`.
fn vitals_metrics_from_hotbar_width(hotbar_w: f32) -> VitalsMetrics {
    let scale = if hotbar_w.is_finite() && hotbar_w > 0.0 {
        hotbar_w / VITALS_TOTAL_WIDTH_BASE
    } else {
        1.0
    };
    VitalsMetrics {
        icon_size: VITALS_ICON_SIZE * scale,
        icon_gap: VITALS_ICON_GAP * scale,
        row_gap: VITALS_ROW_GAP * scale,
        row_width: VITALS_ROW_WIDTH_BASE * scale,
    }
}

/// Top edge of the air row, i.e. the topmost pixel the vitals stack occupies.
///
/// The stack hangs below the selected-name strip band: the heart/hunger row
/// sits one row gap plus one icon below the strip's top edge, and the air row
/// sits the same distance above that. Reserving the strip band unconditionally
/// keeps the vitals from jumping when the transient name appears or fades.
/// Every value is finite on degenerate surfaces because
/// [`selected_name_rect`] and [`vitals_metrics`] both clamp their output.
pub fn vitals_stack_top(surface_w: u32, surface_h: u32) -> f32 {
    let metrics = vitals_metrics(surface_w, surface_h);
    selected_name_rect(surface_w, surface_h).1 - 2.0 * (metrics.row_gap + metrics.icon_size)
}

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

/// Height of the transient selected-item name strip above the hotbar, in px.
pub const SELECTED_NAME_HEIGHT: f32 = 20.0;
/// Vertical gap between the selected-item strip and the hotbar top, in px.
pub const SELECTED_NAME_GAP: f32 = 6.0;

/// Backing rect for the transient selected-item name above the hotbar.
///
/// The strip spans the hotbar's full width and sits one gap above it, which
/// centers it on screen because the hotbar itself is centered. It is clamped
/// into the surface so a tiny or zero-sized surface still yields a finite,
/// on-screen rect (matching [`hotbar_slot_rects`]).
pub fn selected_name_rect(surface_w: u32, surface_h: u32) -> Rect {
    let w_px = surface_w.max(1) as f32;
    let h_px = surface_h.max(1) as f32;
    let rects = hotbar_slot_rects(surface_w, surface_h);
    let (first_x, hotbar_top, _, _) = rects[0];
    let (last_x, _, last_w, _) = rects[HOTBAR_SLOTS - 1];
    let width = ((last_x + last_w) - first_x).clamp(0.0, w_px);
    let height = SELECTED_NAME_HEIGHT.min(h_px);
    let x = first_x.clamp(0.0, (w_px - width).max(0.0));
    let y = (hotbar_top - SELECTED_NAME_GAP - height).clamp(0.0, (h_px - height).max(0.0));
    (x, y, width, height)
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
/// slot state. Only real stacks (count > 1) get a label: tools and single
/// items would otherwise reserve space for a redundant `1` and shrink the
/// icon for no information gain.
pub fn hotbar_count_label_for(inventory: &Inventory, slot: usize) -> String {
    match inventory.hotbar(slot) {
        Some(item) if item.count() > 1 => item.count().to_string(),
        _ => String::new(),
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

/// Icon area for one hotbar slot.
///
/// The icon owns the whole slot so cubes and sprites use the full height;
/// [`icon_rect`](super::item_icon::icon_rect) still applies its deterministic
/// 15% inset. Stack counts are drawn *over* the icon at the bottom-left with a
/// shadow, so they stay legible without shrinking the art. Returning the slot
/// unchanged also keeps the mapping trivially inside the slot for degenerate
/// surfaces.
pub fn hotbar_icon_rect(rect: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    rect
}

/// Fill color of the eating progress bar, distinct from the mining amber.
pub const EATING_FILL_COLOR: [f32; 4] = [0.55, 0.85, 0.35, 0.95];

/// Fill color of the bow-charge progress bar, distinct from both the eating
/// green and the mining amber.
pub const BOW_FILL_COLOR: [f32; 4] = [0.35, 0.85, 1.0, 0.95];

/// Shared crosshair progress-bar geometry: an always-visible dark background
/// followed by a fill of `fill_color` scaled to `progress`.
///
/// NaN collapses to zero and every other value clamps to `0.0..=1.0`.
/// Returns `[background, fill]` so the fill is drawn on top.
fn progress_bar_quads(
    progress: f32,
    surface_w: u32,
    surface_h: u32,
    fill_color: [f32; 4],
) -> [Quad; 2] {
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
        color: fill_color,
    };

    [background, fill]
}

/// Build the two quads that draw the mining progress bar: an always-visible
/// dark background followed by the amber fill scaled to `progress`.
///
/// Returns `[background, fill]` so the fill is drawn on top.
pub fn mining_progress_quads(progress: f32, surface_w: u32, surface_h: u32) -> [Quad; 2] {
    progress_bar_quads(progress, surface_w, surface_h, [0.95, 0.85, 0.25, 0.95])
}

/// Build the two quads that draw the eating progress bar in the same
/// crosshair-anchored slot as the mining bar, with a green fill.
///
/// Sharing the geometry keeps only one bar visible at a time and means a
/// release-cancelled meal never leaves a stale bar shape behind.
pub fn eating_progress_quads(progress: f32, surface_w: u32, surface_h: u32) -> [Quad; 2] {
    progress_bar_quads(progress, surface_w, surface_h, EATING_FILL_COLOR)
}

/// Build the two quads that draw the bow-charge progress bar in the same
/// crosshair-anchored slot as the mining and eating bars, with a cyan fill.
///
/// The slot is shared, so only one action bar is ever visible at a time: the
/// caller's priority (eating, then bow, then mining) picks which action owns
/// this frame. Sharing the geometry also means a cancelled draw never leaves
/// a stale bar shape behind.
pub fn bow_progress_quads(progress: f32, surface_w: u32, surface_h: u32) -> [Quad; 2] {
    progress_bar_quads(progress, surface_w, surface_h, BOW_FILL_COLOR)
}

/// Fill fraction of one heart icon: ten hearts of two health points each.
///
/// An odd health value shows a half heart (`0.5`); indices past the tenth
/// heart (and any index the row cannot draw) return `0.0`. The fill never
/// exceeds `1.0` even when a caller passes health above the gameplay maximum.
pub fn heart_fill(health: u16, index: usize) -> f32 {
    icon_fill(health, index, MAX_HEALTH)
}

/// Fill fraction of one hunger pip: ten pips of two hunger points each.
///
/// Shares the heart rule — an odd value shows a half pip and out-of-range
/// indices return `0.0` — so both rows stay visually symmetric.
pub fn hunger_fill(hunger: u16, index: usize) -> f32 {
    icon_fill(hunger, index, MAX_HUNGER)
}

/// Shared two-points-per-icon fill rule for hearts and hunger pips.
fn icon_fill(points: u16, index: usize, max_points: u16) -> f32 {
    let icons = usize::from(max_points / 2);
    if index >= icons {
        return 0.0;
    }
    let consumed = (index as u16).saturating_mul(2);
    let remaining = points.saturating_sub(consumed).min(2);
    f32::from(remaining) / 2.0
}

/// Number of air bubbles to draw for `air` points: ten bubbles scale linearly
/// with the [`FULL_AIR`] lung, rounding up so any remaining breath is visible.
///
/// Integer arithmetic keeps exact boundaries exact (`30` points is exactly one
/// bubble) and the result is clamped to ten; a save above `FULL_AIR` never
/// draws more than a full row.
pub fn air_bubbles(air: u16) -> u8 {
    const BUBBLE_COUNT: u32 = 10;
    let scaled = u32::from(air) * BUBBLE_COUNT;
    let bubbles = scaled.div_ceil(u32::from(FULL_AIR));
    bubbles.min(BUBBLE_COUNT) as u8
}

/// Pixel-space vitals HUD quads for one frame.
///
/// Creative is structurally exempt: it draws nothing even when vitals are low
/// or the head is submerged. Survival draws ten hearts anchored to the first
/// hotbar slot's left edge and ten hunger pips anchored to the last hotbar
/// slot's right edge, both one row above the selected-name strip band. When
/// the head is submerged or the lungs are not full, ten air bubbles appear one
/// row above the hunger row, right-aligned to the same hotbar edge.
///
/// The strip band is reserved unconditionally (see [`vitals_stack_top`]), so
/// the vitals never jump when the transient selected name appears or fades,
/// and the strip can never paint over hearts or hunger.
///
/// Every icon gets a dark background quad so an empty row is still legible;
/// a filled icon adds a colored quad scaled by its fill fraction (a half heart
/// is half-width). A zero fill emits no fill quad because it would draw
/// nothing.
///
/// All geometry derives from [`hotbar_slot_rects`], whose `max(1)` convention
/// keeps every coordinate finite on a zero-sized surface.
pub fn vitals_quads(
    vitals: &SurvivalState,
    mode: GameMode,
    head_submerged: bool,
    surface_w: u32,
    surface_h: u32,
) -> Vec<Quad> {
    if mode != GameMode::Survival {
        return Vec::new();
    }

    let rects = hotbar_slot_rects(surface_w, surface_h);
    let (first_x, _, _, _) = rects[0];
    let (last_x, _, last_w, _) = rects[HOTBAR_SLOTS - 1];
    let right_edge = last_x + last_w;
    let metrics = vitals_metrics_from_hotbar_width(right_edge - first_x);

    // Hunger and air are right-aligned to the hotbar's right edge; hearts are
    // left-aligned to its left edge. Both rows share the same ten-icon width.
    // The stack hangs below the selected-name strip band, not the hotbar: the
    // strip is painted later, so anchoring here keeps hearts and hunger visible
    // whether or not the transient name is showing. The icon size, gap, and row
    // height all scale from the measured hotbar width, so the two rows can
    // never overlap.
    let strip_top = selected_name_rect(surface_w, surface_h).1;
    let heart_y = strip_top - metrics.row_gap - metrics.icon_size;
    let air_y = heart_y - metrics.row_gap - metrics.icon_size;
    let hunger_x = right_edge - metrics.row_width;
    let step = metrics.icon_size + metrics.icon_gap;

    let mut quads = Vec::with_capacity(VITALS_ICON_COUNT * 6);
    for index in 0..VITALS_ICON_COUNT {
        push_vitals_icon(
            &mut quads,
            first_x + index as f32 * step,
            heart_y,
            metrics.icon_size,
            heart_fill(vitals.health(), index),
            HEART_FILL_COLOR,
        );
    }
    for index in 0..VITALS_ICON_COUNT {
        push_vitals_icon(
            &mut quads,
            hunger_x + index as f32 * step,
            heart_y,
            metrics.icon_size,
            hunger_fill(vitals.hunger(), index),
            HUNGER_FILL_COLOR,
        );
    }
    if head_submerged || vitals.air() < FULL_AIR {
        let bubbles = usize::from(air_bubbles(vitals.air()));
        for index in 0..VITALS_ICON_COUNT {
            let fill = if index < bubbles { 1.0 } else { 0.0 };
            push_vitals_icon(
                &mut quads,
                hunger_x + index as f32 * step,
                air_y,
                metrics.icon_size,
                fill,
                AIR_FILL_COLOR,
            );
        }
    }

    quads
}

/// Build the optional full-screen red damage flash quad for `intensity`.
///
/// `intensity` is the player combat carrier's `hurt_intensity()` (the
/// remaining 0.5 s flash scaled to `0.0..=1.0`). A non-finite or non-positive
/// intensity returns `None`, so a quiescent or corrupted timer draws nothing
/// rather than a stray black/NaN rectangle; every other value clamps into
/// `0.0..=1.0` before it scales [`DAMAGE_FLASH_ALPHA`].
///
/// The quad covers the whole surface in physical pixels (`x/y = 0`), the same
/// coordinate space as every other HUD quad. It is appended after the vitals
/// quads by the caller, so the tint covers the HUD too.
pub fn damage_flash_quad(intensity: f32, surface_w: u32, surface_h: u32) -> Option<Quad> {
    if !intensity.is_finite() || intensity <= 0.0 {
        return None;
    }
    Some(Quad {
        x: 0.0,
        y: 0.0,
        w: surface_w.max(1) as f32,
        h: surface_h.max(1) as f32,
        color: [
            0.8,
            0.05,
            0.05,
            DAMAGE_FLASH_ALPHA * intensity.clamp(0.0, 1.0),
        ],
    })
}

/// One vitals icon: an always-present dark background plus an optional colored
/// fill scaled horizontally by `fill` (odd hearts/hunger show half an icon).
///
/// `icon_size` is the hotbar-scaled icon edge; both the background and the
/// half/full fill use it so a row keeps one proportional size.
fn push_vitals_icon(
    quads: &mut Vec<Quad>,
    x: f32,
    y: f32,
    icon_size: f32,
    fill: f32,
    fill_color: [f32; 4],
) {
    quads.push(Quad {
        x,
        y,
        w: icon_size,
        h: icon_size,
        color: VITALS_BACKGROUND_COLOR,
    });
    let fill = if fill.is_finite() {
        fill.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if fill > 0.0 {
        quads.push(Quad {
            x,
            y,
            w: icon_size * fill,
            h: icon_size,
            color: fill_color,
        });
    }
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

    // -----------------------------------------------------------------------
    // Eating progress bar (S05-T02)
    // -----------------------------------------------------------------------

    #[test]
    fn eating_and_mining_bars_share_the_same_geometry() {
        for progress in [0.0, 0.25, 0.5, 1.0, 1.5, -1.0] {
            let [eat_bg, eat_fill] = eating_progress_quads(progress, 1280, 720);
            let [mine_bg, mine_fill] = mining_progress_quads(progress, 1280, 720);
            for (name, eat, mine) in [
                ("background", eat_bg, mine_bg),
                ("fill", eat_fill, mine_fill),
            ] {
                assert_close(eat.x, mine.x, &format!("{name} x at {progress}"));
                assert_close(eat.y, mine.y, &format!("{name} y at {progress}"));
                assert_close(eat.w, mine.w, &format!("{name} w at {progress}"));
                assert_close(eat.h, mine.h, &format!("{name} h at {progress}"));
            }
        }
    }

    #[test]
    fn eating_fill_color_is_distinct_and_valid() {
        let [eat_bg, eat_fill] = eating_progress_quads(0.5, 1280, 720);
        let [mine_bg, mine_fill] = mining_progress_quads(0.5, 1280, 720);

        assert_eq!(eat_fill.color, EATING_FILL_COLOR, "eating fill color");
        assert_eq!(
            mine_fill.color,
            [0.95, 0.85, 0.25, 0.95],
            "mining stays amber"
        );
        assert_eq!(eat_bg.color, mine_bg.color, "shared dark background");
        assert_eq!(eat_bg.color, [0.0, 0.0, 0.0, 0.55], "background stays dark");

        assert_ne!(EATING_FILL_COLOR, mine_fill.color, "distinct from mining");
        for (name, other) in [
            ("heart", HEART_FILL_COLOR),
            ("hunger", HUNGER_FILL_COLOR),
            ("air", AIR_FILL_COLOR),
            ("vitals background", VITALS_BACKGROUND_COLOR),
            ("progress background", eat_bg.color),
        ] {
            assert_ne!(EATING_FILL_COLOR, other, "eating fill differs from {name}");
        }
        for channel in EATING_FILL_COLOR {
            assert!(
                (0.0..=1.0).contains(&channel),
                "eating channel {channel} out of range"
            );
        }
    }

    #[test]
    fn eating_progress_clamps_to_unit_range() {
        let [_, full] = eating_progress_quads(1.5, 1280, 720);
        assert_close(full.w, PROGRESS_BAR_WIDTH, "1.5 is full width");

        let [_, infinite] = eating_progress_quads(f32::INFINITY, 1280, 720);
        assert_close(infinite.w, PROGRESS_BAR_WIDTH, "infinity is full width");

        let [_, negative] = eating_progress_quads(-1.0, 1280, 720);
        assert_close(negative.w, 0.0, "-1.0 is zero width");

        let [_, nan] = eating_progress_quads(f32::NAN, 1280, 720);
        assert_close(nan.w, 0.0, "NaN is zero width");
    }

    #[test]
    fn eating_bar_stays_finite_on_degenerate_surfaces() {
        for (w, h) in [(0, 0), (1, 1)] {
            for quad in eating_progress_quads(0.5, w, h) {
                for (name, value) in [("x", quad.x), ("y", quad.y), ("w", quad.w), ("h", quad.h)] {
                    assert!(value.is_finite(), "{name} finite on {w}x{h}");
                }
            }
        }

        let [zero, _] = eating_progress_quads(0.5, 0, 0);
        let [one, _] = eating_progress_quads(0.5, 1, 1);
        assert_close(zero.x, one.x, "0x0 centers like 1x1");
        assert_close(zero.y, one.y, "0x0 centers like 1x1");
        assert_close(zero.x, (1.0 - PROGRESS_BAR_WIDTH) / 2.0, "max(1) x");
        assert_close(zero.y, 0.5 + CROSSHAIR_BAR_OFFSET_Y, "max(1) y");
        assert_close(zero.w, PROGRESS_BAR_WIDTH, "background width intact");
        assert_close(zero.h, PROGRESS_BAR_HEIGHT, "background height intact");
    }

    // -----------------------------------------------------------------------
    // Bow-charge progress bar (S07-T05)
    // -----------------------------------------------------------------------

    #[test]
    fn bow_bar_shares_the_geometry_and_scales_with_progress() {
        for (progress, expected_w) in [(0.0, 0.0), (0.25, 30.0), (0.5, 60.0), (1.0, 120.0)] {
            let [bow_bg, bow_fill] = bow_progress_quads(progress, 1280, 720);
            let [mine_bg, mine_fill] = mining_progress_quads(progress, 1280, 720);
            for (name, bow, mine) in [
                ("background", bow_bg, mine_bg),
                ("fill", bow_fill, mine_fill),
            ] {
                assert_close(bow.x, mine.x, &format!("{name} x at {progress}"));
                assert_close(bow.y, mine.y, &format!("{name} y at {progress}"));
                assert_close(bow.h, mine.h, &format!("{name} h at {progress}"));
            }
            assert_close(bow_fill.w, expected_w, &format!("fill w at {progress}"));
        }
    }

    #[test]
    fn bow_fill_color_is_distinct_from_eating_and_mining() {
        let [bow_bg, bow_fill] = bow_progress_quads(0.5, 1280, 720);
        let [mine_bg, mine_fill] = mining_progress_quads(0.5, 1280, 720);
        let [eat_bg, eat_fill] = eating_progress_quads(0.5, 1280, 720);

        assert_eq!(bow_fill.color, BOW_FILL_COLOR, "bow fill color");
        assert_eq!(bow_bg.color, mine_bg.color, "shared dark background");
        assert_eq!(bow_bg.color, eat_bg.color, "shared dark background");
        assert_eq!(bow_bg.color, [0.0, 0.0, 0.0, 0.55], "background stays dark");

        assert_ne!(BOW_FILL_COLOR, EATING_FILL_COLOR, "distinct from eating");
        assert_ne!(BOW_FILL_COLOR, mine_fill.color, "distinct from mining");
        assert_ne!(eat_fill.color, mine_fill.color, "eating stays distinct");
        for channel in BOW_FILL_COLOR {
            assert!(
                (0.0..=1.0).contains(&channel),
                "bow channel {channel} out of range"
            );
        }
    }

    #[test]
    fn bow_bar_clamps_and_stays_finite_on_degenerate_surfaces() {
        let [_, full] = bow_progress_quads(1.5, 1280, 720);
        assert_close(full.w, PROGRESS_BAR_WIDTH, "1.5 is full width");
        let [_, nan] = bow_progress_quads(f32::NAN, 1280, 720);
        assert_close(nan.w, 0.0, "NaN is zero width");

        for (w, h) in [(0, 0), (1, 1)] {
            for quad in bow_progress_quads(0.5, w, h) {
                for (name, value) in [("x", quad.x), ("y", quad.y), ("w", quad.w), ("h", quad.h)] {
                    assert!(value.is_finite(), "{name} finite on {w}x{h}");
                }
            }
        }
    }

    #[test]
    fn hotbar_icon_rect_uses_the_full_slot_so_counts_can_overlay() {
        for (w, h) in [(1280, 720), (0, 0), (80, 60)] {
            for (slot, rect) in hotbar_slot_rects(w, h).iter().enumerate() {
                let icon = hotbar_icon_rect(*rect);
                let label_top = hotbar_count_text_origin(*rect).1;
                assert_eq!(icon, *rect, "slot {slot} icon owns the full slot");
                for (name, value) in [("x", icon.0), ("y", icon.1), ("w", icon.2), ("h", icon.3)] {
                    assert!(value.is_finite(), "slot {slot} icon {name} finite");
                }
                assert!(icon.2 >= 0.0 && icon.3 >= 0.0, "never inverted");
                // The icon now intentionally reaches past the old label top;
                // the label is drawn afterward as an overlay with a shadow.
                assert!(
                    icon.1 + icon.3 >= label_top,
                    "slot {slot} icon uses the full height above the count origin"
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

        // Count one is not a stack label: tools and single items keep the
        // full slot for the icon instead of reserving space for a redundant
        // `1`.
        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(Item::new(ItemId::Material(Material::Apple), 1).expect("apple"));
        let singles = Inventory::from_parts(slots, None);
        assert!(hotbar_count_label_for(&singles, 0).is_empty());

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

    // -----------------------------------------------------------------------
    // Vitals HUD (S04-T06)
    // -----------------------------------------------------------------------

    fn count_color(quads: &[Quad], color: [f32; 4]) -> usize {
        quads.iter().filter(|quad| quad.color == color).count()
    }

    #[test]
    fn heart_fill_splits_two_points_per_icon() {
        for index in 0..10 {
            assert_eq!(heart_fill(0, index), 0.0, "zero health index {index}");
        }
        assert_eq!(heart_fill(1, 0), 0.5, "odd health shows a half heart");
        assert_eq!(heart_fill(1, 1), 0.0);
        assert_eq!(heart_fill(2, 0), 1.0);
        assert_eq!(heart_fill(2, 1), 0.0);
        assert_eq!(heart_fill(19, 8), 1.0, "16 points consumed by icons 0..8");
        assert_eq!(heart_fill(19, 9), 0.5, "the last icon holds the odd point");
        assert_eq!(heart_fill(20, 9), 1.0);
        assert_eq!(heart_fill(20, 10), 0.0, "no eleventh heart");
        assert_eq!(heart_fill(20, usize::MAX), 0.0, "out-of-range is empty");
        assert_eq!(heart_fill(99, 0), 1.0, "fills never exceed one icon");
    }

    #[test]
    fn hunger_fill_splits_two_points_per_pip() {
        for index in 0..10 {
            assert_eq!(hunger_fill(0, index), 0.0, "zero hunger index {index}");
        }
        assert_eq!(hunger_fill(1, 0), 0.5);
        assert_eq!(hunger_fill(1, 1), 0.0);
        assert_eq!(hunger_fill(2, 0), 1.0);
        assert_eq!(hunger_fill(2, 1), 0.0);
        assert_eq!(hunger_fill(19, 8), 1.0);
        assert_eq!(hunger_fill(19, 9), 0.5);
        assert_eq!(hunger_fill(20, 9), 1.0);
        assert_eq!(hunger_fill(20, 10), 0.0);
        assert_eq!(hunger_fill(20, usize::MAX), 0.0);
        assert_eq!(hunger_fill(99, 0), 1.0);
    }

    #[test]
    fn air_bubbles_round_up_and_clamp_to_ten() {
        assert_eq!(air_bubbles(0), 0);
        assert_eq!(air_bubbles(1), 1, "any remaining breath shows one bubble");
        assert_eq!(air_bubbles(30), 1, "exact boundary stays exactly one");
        assert_eq!(air_bubbles(150), 5, "half a lung is five bubbles");
        assert_eq!(air_bubbles(299), 10, "rounding up fills the last bubble");
        assert_eq!(air_bubbles(300), 10);
        assert_eq!(air_bubbles(1200), 10, "a save above FULL_AIR clamps");
    }

    #[test]
    fn creative_draws_no_vitals_even_when_hurt_or_submerged() {
        let low = SurvivalState::new(1, 0, 0).expect("valid vitals");
        assert!(
            vitals_quads(&low, GameMode::Creative, true, 1280, 720).is_empty(),
            "Creative is structurally exempt from the vitals HUD"
        );
        assert!(vitals_quads(&SurvivalState::full(), GameMode::Creative, false, 0, 0).is_empty());
    }

    #[test]
    fn survival_full_and_dry_draws_hearts_and_hunger_but_no_air() {
        let quads = vitals_quads(&SurvivalState::full(), GameMode::Survival, false, 1280, 720);
        assert_eq!(count_color(&quads, HEART_FILL_COLOR), 10);
        assert_eq!(count_color(&quads, HUNGER_FILL_COLOR), 10);
        assert_eq!(count_color(&quads, AIR_FILL_COLOR), 0, "full lungs on land");
        assert_eq!(count_color(&quads, VITALS_BACKGROUND_COLOR), 20);
    }

    #[test]
    fn survival_submerged_or_low_air_draws_the_air_row() {
        let full = SurvivalState::full();
        let submerged = vitals_quads(&full, GameMode::Survival, true, 1280, 720);
        assert_eq!(count_color(&submerged, AIR_FILL_COLOR), 10);
        assert_eq!(count_color(&submerged, VITALS_BACKGROUND_COLOR), 30);

        // Low breath shows the row even while the head is above water: one
        // bubble for 30/300 points, and the row still has all ten backgrounds.
        let low = SurvivalState::new(20, 20, 30).expect("valid vitals");
        let surface = vitals_quads(&low, GameMode::Survival, false, 1280, 720);
        assert_eq!(count_color(&surface, AIR_FILL_COLOR), 1);
        assert_eq!(count_color(&surface, VITALS_BACKGROUND_COLOR), 30);

        // Empty lungs on land still draw the dark row (no colored bubble).
        let empty = SurvivalState::new(20, 20, 0).expect("valid vitals");
        let surface = vitals_quads(&empty, GameMode::Survival, false, 1280, 720);
        assert_eq!(count_color(&surface, AIR_FILL_COLOR), 0);
        assert_eq!(count_color(&surface, VITALS_BACKGROUND_COLOR), 30);
    }

    #[test]
    fn empty_icons_stay_dark_and_half_icons_are_half_width() {
        let empty = SurvivalState::new(0, 0, FULL_AIR).expect("valid vitals");
        let quads = vitals_quads(&empty, GameMode::Survival, false, 1280, 720);
        assert_eq!(count_color(&quads, HEART_FILL_COLOR), 0);
        assert_eq!(count_color(&quads, HUNGER_FILL_COLOR), 0);
        assert_eq!(
            count_color(&quads, VITALS_BACKGROUND_COLOR),
            20,
            "empty icons are still drawn dark"
        );

        let one = SurvivalState::new(1, 1, FULL_AIR).expect("valid vitals");
        let quads = vitals_quads(&one, GameMode::Survival, false, 1280, 720);
        let icon_size = vitals_metrics(1280, 720).icon_size;
        let hearts: Vec<_> = quads
            .iter()
            .filter(|quad| quad.color == HEART_FILL_COLOR)
            .collect();
        assert_eq!(hearts.len(), 1, "one half heart");
        assert_close(hearts[0].w, icon_size * 0.5, "half-heart width");
        let hunger: Vec<_> = quads
            .iter()
            .filter(|quad| quad.color == HUNGER_FILL_COLOR)
            .collect();
        assert_eq!(hunger.len(), 1, "one half hunger pip");
        assert_close(hunger[0].w, icon_size * 0.5, "half-pip width");
    }

    #[test]
    fn vitals_rows_sit_above_the_hotbar_and_align_with_its_edges() {
        let surface_w = 1280;
        let surface_h = 720;
        let rects = hotbar_slot_rects(surface_w, surface_h);
        let hotbar_top = rects[0].1;
        let last_right = rects[HOTBAR_SLOTS - 1].0 + rects[HOTBAR_SLOTS - 1].2;
        let hotbar_w = last_right - rects[0].0;
        let metrics = vitals_metrics(surface_w, surface_h);
        let strip_top = selected_name_rect(surface_w, surface_h).1;
        let quads = vitals_quads(
            &SurvivalState::full(),
            GameMode::Survival,
            true,
            surface_w,
            surface_h,
        );

        for quad in &quads {
            assert!(
                quad.y + quad.h <= hotbar_top + 1e-4,
                "every vitals row sits above the hotbar top"
            );
            assert!(
                quad.y + quad.h <= strip_top + 1e-3,
                "every vitals quad clears the selected-name strip top {strip_top}"
            );
        }

        let hearts: Vec<_> = quads
            .iter()
            .filter(|quad| quad.color == HEART_FILL_COLOR)
            .collect();
        assert_eq!(hearts.len(), 10);
        let hearts_top = hearts.iter().map(|q| q.y).fold(f32::MAX, f32::min);
        assert_close(
            hearts_top,
            strip_top - metrics.row_gap - metrics.icon_size,
            "hearts sit one row gap and one icon below the strip top",
        );
        let hearts_left = hearts.iter().map(|q| q.x).fold(f32::MAX, f32::min);
        let hearts_right = hearts.iter().map(|q| q.x + q.w).fold(f32::MIN, f32::max);
        assert_close(hearts_left, rects[0].0, "hearts left-aligned to slot 0");
        assert_close(
            hearts_right,
            rects[0].0 + metrics.row_width,
            "ten hearts span one icon row",
        );

        let hunger: Vec<_> = quads
            .iter()
            .filter(|quad| quad.color == HUNGER_FILL_COLOR)
            .collect();
        let hunger_right = hunger.iter().map(|q| q.x + q.w).fold(f32::MIN, f32::max);
        assert_close(
            hunger_right,
            last_right,
            "hunger right edge is slot 8's right edge",
        );
        let hunger_left = hunger.iter().map(|q| q.x).fold(f32::MAX, f32::min);
        assert_close(
            hunger_left,
            last_right - metrics.row_width,
            "hunger row is right-aligned as one block",
        );
        assert!(
            hearts_right + metrics.icon_gap <= hunger_left + 1e-3,
            "a visible center gap separates hearts from hunger: {hearts_right} + {} > {hunger_left}",
            metrics.icon_gap
        );
        assert!(
            metrics.row_width * 2.0 + metrics.icon_gap <= hotbar_w + 1e-3,
            "both scaled rows fit inside the measured hotbar width: {} > {hotbar_w}",
            metrics.row_width * 2.0 + metrics.icon_gap
        );

        let air: Vec<_> = quads
            .iter()
            .filter(|quad| quad.color == AIR_FILL_COLOR)
            .collect();
        let air_bottom = air.iter().map(|q| q.y + q.h).fold(f32::MIN, f32::max);
        let hunger_top = hunger.iter().map(|q| q.y).fold(f32::MAX, f32::min);
        assert!(
            air_bottom < hunger_top,
            "the air row sits above the hunger row"
        );
    }

    #[test]
    fn vitals_stack_hangs_below_the_selected_name_strip_on_every_surface() {
        for (surface_w, surface_h) in [(640u32, 480u32), (1280, 720), (1920, 1080), (720, 1280)] {
            let strip = selected_name_rect(surface_w, surface_h);
            let metrics = vitals_metrics(surface_w, surface_h);
            let quads = vitals_quads(
                &SurvivalState::full(),
                GameMode::Survival,
                true,
                surface_w,
                surface_h,
            );
            assert_eq!(
                quads.len(),
                VITALS_ICON_COUNT * 6,
                "full vitals and a submerged head draw hearts, hunger, and air"
            );

            let hearts: Vec<_> = quads
                .iter()
                .filter(|quad| quad.color == HEART_FILL_COLOR)
                .collect();
            let hearts_top = hearts.iter().map(|q| q.y).fold(f32::MAX, f32::min);
            assert_close(
                hearts_top,
                strip.1 - metrics.row_gap - metrics.icon_size,
                "hearts top anchors to the reserved strip band",
            );

            let air: Vec<_> = quads
                .iter()
                .filter(|quad| quad.color == AIR_FILL_COLOR)
                .collect();
            let hunger: Vec<_> = quads
                .iter()
                .filter(|quad| quad.color == HUNGER_FILL_COLOR)
                .collect();
            let air_bottom = air.iter().map(|q| q.y + q.h).fold(f32::MIN, f32::max);
            let hunger_top = hunger.iter().map(|q| q.y).fold(f32::MAX, f32::min);
            assert!(
                air_bottom < hunger_top,
                "air row stays on its own line above hunger on {surface_w}x{surface_h}"
            );

            let surface_w_px = surface_w as f32;
            let surface_h_px = surface_h as f32;
            for quad in &quads {
                assert!(
                    quad.y + quad.h <= strip.1 + 1e-3,
                    "quad bottom {} covers the strip top {} on {surface_w}x{surface_h}",
                    quad.y + quad.h,
                    strip.1
                );
                assert!(
                    quad.x >= 0.0 && quad.x + quad.w <= surface_w_px + 1e-3,
                    "quad x span {}..{} leaves {surface_w}x{surface_h}",
                    quad.x,
                    quad.x + quad.w
                );
                assert!(
                    quad.y >= 0.0 && quad.y + quad.h <= surface_h_px + 1e-3,
                    "quad y span {}..{} leaves {surface_w}x{surface_h}",
                    quad.y,
                    quad.y + quad.h
                );
            }
        }
    }

    #[test]
    fn vitals_stack_stays_finite_on_degenerate_surfaces() {
        for (surface_w, surface_h) in [(0u32, 0u32), (1, 1), (8, 8)] {
            let stack_top = vitals_stack_top(surface_w, surface_h);
            assert!(
                stack_top.is_finite(),
                "stack top finite on {surface_w}x{surface_h}"
            );
            let metrics = vitals_metrics(surface_w, surface_h);
            assert!(metrics.icon_size.is_finite() && metrics.icon_size >= 0.0);
            assert!(metrics.icon_gap.is_finite() && metrics.icon_gap >= 0.0);
            assert!(metrics.row_gap.is_finite() && metrics.row_gap >= 0.0);
            assert!(metrics.row_width.is_finite() && metrics.row_width >= 0.0);

            // Negative y is acceptable on a surface too small for the stack;
            // the vitals must stay finite and panic-free, not be clamped into
            // the hotbar.
            let quads = vitals_quads(
                &SurvivalState::full(),
                GameMode::Survival,
                true,
                surface_w,
                surface_h,
            );
            for quad in &quads {
                for (name, value) in [("x", quad.x), ("y", quad.y), ("w", quad.w), ("h", quad.h)] {
                    assert!(
                        value.is_finite(),
                        "{name} finite on {surface_w}x{surface_h}"
                    );
                }
            }
        }
    }

    #[test]
    fn vitals_metrics_never_overlap_across_supported_resolutions() {
        // The acceptance contract S07's smoke and UAT rely on: at every
        // supported surface the scaled vitals stack fits the measured hotbar,
        // hearts and hunger stay separated by a visible gap, the air row keeps
        // its own line, and no quad covers the selected-name strip band.
        const SURFACES: [(u32, u32); 9] = [
            (640, 480),
            (800, 600),
            (1280, 720),
            (1366, 768),
            (1920, 1080),
            (2560, 1440),
            (3840, 2160),
            (720, 1280),
            (1080, 1920),
        ];

        for (surface_w, surface_h) in SURFACES {
            let rects = hotbar_slot_rects(surface_w, surface_h);
            let hotbar_w = (rects[HOTBAR_SLOTS - 1].0 + rects[HOTBAR_SLOTS - 1].2) - rects[0].0;
            let metrics = vitals_metrics(surface_w, surface_h);
            assert!(
                metrics.row_width * 2.0 + metrics.icon_gap <= hotbar_w + 1e-3,
                "both scaled rows fit inside the hotbar at {surface_w}x{surface_h}: {} > {hotbar_w}",
                metrics.row_width * 2.0 + metrics.icon_gap
            );

            let quads = vitals_quads(
                &SurvivalState::full(),
                GameMode::Survival,
                true,
                surface_w,
                surface_h,
            );

            let hearts: Vec<_> = quads
                .iter()
                .filter(|quad| quad.color == HEART_FILL_COLOR)
                .collect();
            let hunger: Vec<_> = quads
                .iter()
                .filter(|quad| quad.color == HUNGER_FILL_COLOR)
                .collect();
            assert_eq!(
                hearts.len(),
                10,
                "ten heart fills on {surface_w}x{surface_h}"
            );
            assert_eq!(
                hunger.len(),
                10,
                "ten hunger fills on {surface_w}x{surface_h}"
            );

            let hearts_left = hearts.iter().map(|q| q.x).fold(f32::MAX, f32::min);
            let hearts_right = hearts.iter().map(|q| q.x + q.w).fold(f32::MIN, f32::max);
            let hunger_left = hunger.iter().map(|q| q.x).fold(f32::MAX, f32::min);
            assert!(
                (hearts_left - rects[0].0).abs() < 1e-3,
                "hearts left-align to slot 0 at {surface_w}x{surface_h}: {hearts_left} != {}",
                rects[0].0
            );
            assert!(
                hunger_left - hearts_right >= metrics.icon_gap - 1e-3,
                "visible center gap at {surface_w}x{surface_h}: {} - {} < {}",
                hunger_left,
                hearts_right,
                metrics.icon_gap
            );

            let strip_top = selected_name_rect(surface_w, surface_h).1;
            let surface_w_px = surface_w as f32;
            let surface_h_px = surface_h as f32;
            for quad in &quads {
                assert!(
                    quad.x >= 0.0,
                    "quad x {} leaves the surface at {surface_w}x{surface_h}",
                    quad.x
                );
                assert!(
                    quad.x + quad.w <= surface_w_px + 1e-3,
                    "quad right {} leaves the surface at {surface_w}x{surface_h}",
                    quad.x + quad.w
                );
                assert!(
                    quad.y >= 0.0,
                    "quad y {} leaves the surface at {surface_w}x{surface_h}",
                    quad.y
                );
                assert!(
                    quad.y + quad.h <= surface_h_px + 1e-3,
                    "quad bottom {} leaves the surface at {surface_w}x{surface_h}",
                    quad.y + quad.h
                );
                assert!(
                    quad.y + quad.h <= strip_top + 1e-3,
                    "quad bottom {} covers the strip top {strip_top} at {surface_w}x{surface_h}",
                    quad.y + quad.h
                );
            }

            let air: Vec<_> = quads
                .iter()
                .filter(|quad| quad.color == AIR_FILL_COLOR)
                .collect();
            assert_eq!(air.len(), 10, "ten air fills on {surface_w}x{surface_h}");
            let air_bottom = air.iter().map(|q| q.y + q.h).fold(f32::MIN, f32::max);
            let hunger_top = hunger.iter().map(|q| q.y).fold(f32::MAX, f32::min);
            assert!(
                air_bottom < hunger_top,
                "air keeps its own line above hunger at {surface_w}x{surface_h}: {air_bottom} !< {hunger_top}"
            );
        }

        // Pinned geometry: the same continuous ratio must produce these exact
        // scaled sizes, so a future constant change cannot silently drift.
        for (surface_w, surface_h, icon, row_width) in [
            (1280u32, 720u32, 14.5770f32, 162.1689f32),
            (1920, 1080, 21.8655, 243.2534),
            (3840, 2160, 43.7309, 486.5068),
        ] {
            let metrics = vitals_metrics(surface_w, surface_h);
            assert!(
                (metrics.icon_size - icon).abs() < 1e-3,
                "pinned icon size at {surface_w}x{surface_h}: {} != {icon}",
                metrics.icon_size
            );
            assert!(
                (metrics.row_width - row_width).abs() < 1e-3,
                "pinned row width at {surface_w}x{surface_h}: {} != {row_width}",
                metrics.row_width
            );
        }

        // 16:9 growth is strict and proportional: bigger surfaces scale the
        // whole stack, not just some fields.
        let mut previous: Option<(f32, f32)> = None;
        for height in [720u32, 1080, 1440, 2160] {
            let width = height / 9 * 16;
            let metrics = vitals_metrics(width, height);
            let ratio = metrics.icon_size / height as f32;
            if let Some((previous_icon, previous_ratio)) = previous {
                assert!(
                    metrics.icon_size > previous_icon,
                    "icon size must grow at 16:9 height {height}: {} !> {previous_icon}",
                    metrics.icon_size
                );
                assert!(
                    (ratio - previous_ratio).abs() < 1e-4,
                    "icon/height ratio must stay constant at 16:9 height {height}: {ratio} != {previous_ratio}"
                );
            }
            previous = Some((metrics.icon_size, ratio));
        }
    }

    #[test]
    fn vitals_metrics_stay_finite_on_portrait_and_tiny_surfaces() {
        // Portrait and degenerate surfaces are finite-only: the vitals may sit
        // off-screen on a 1x1 surface, but they must never panic or go NaN.
        for (surface_w, surface_h) in [(400u32, 800u32), (0, 0), (1, 1), (8, 8)] {
            let metrics = vitals_metrics(surface_w, surface_h);
            assert!(
                metrics.icon_size.is_finite(),
                "icon size finite on {surface_w}x{surface_h}"
            );
            assert!(
                metrics.icon_gap.is_finite() && metrics.icon_gap >= 0.0,
                "icon gap finite and non-negative on {surface_w}x{surface_h}"
            );
            assert!(
                metrics.row_gap.is_finite() && metrics.row_gap >= 0.0,
                "row gap finite and non-negative on {surface_w}x{surface_h}"
            );
            assert!(
                metrics.row_width.is_finite() && metrics.row_width >= 0.0,
                "row width finite and non-negative on {surface_w}x{surface_h}"
            );

            let quads = vitals_quads(
                &SurvivalState::full(),
                GameMode::Survival,
                true,
                surface_w,
                surface_h,
            );
            assert!(
                !quads.is_empty(),
                "vitals still draw on {surface_w}x{surface_h}"
            );
            for quad in &quads {
                for (name, value) in [("x", quad.x), ("y", quad.y), ("w", quad.w), ("h", quad.h)] {
                    assert!(
                        value.is_finite(),
                        "{name} finite on {surface_w}x{surface_h}"
                    );
                }
            }
        }
    }

    #[test]
    fn vitals_quads_stay_finite_on_degenerate_surfaces() {
        for (w, h) in [(0, 0), (1, 1), (8, 8)] {
            for mode in [GameMode::Survival, GameMode::Creative] {
                let quads = vitals_quads(&SurvivalState::full(), mode, true, w, h);
                for quad in &quads {
                    for (name, value) in
                        [("x", quad.x), ("y", quad.y), ("w", quad.w), ("h", quad.h)]
                    {
                        assert!(value.is_finite(), "{name} finite on {w}x{h}");
                    }
                    for channel in quad.color {
                        assert!(channel.is_finite(), "color finite on {w}x{h}");
                    }
                }
            }
        }
        // Zero vitals, zero surface: still no panic and finite output.
        let zero = SurvivalState::new(0, 0, 0).expect("valid vitals");
        let quads = vitals_quads(&zero, GameMode::Survival, true, 0, 0);
        assert!(!quads.is_empty());
        for quad in quads {
            assert!(quad.x.is_finite() && quad.y.is_finite());
        }
    }

    // -----------------------------------------------------------------------
    // Selected-item name strip (S05-T01)
    // -----------------------------------------------------------------------

    #[test]
    fn selected_name_rect_sits_above_the_hotbar_and_stays_on_screen() {
        for (w, h) in [(1280, 720), (1920, 1080), (720, 1280), (1080, 1920)] {
            let rect = selected_name_rect(w, h);
            let hotbar_top = hotbar_slot_rects(w, h)[0].1;
            let (x, y, rw, rh) = rect;
            for (name, value) in [("x", x), ("y", y), ("w", rw), ("h", rh)] {
                assert!(value.is_finite(), "{name} finite on {w}x{h}");
            }
            assert!(rw >= 0.0 && rh >= 0.0, "positive strip on {w}x{h}");
            assert!(x >= 0.0 && y >= 0.0, "starts on screen at {w}x{h}");
            assert!(
                x + rw <= w as f32 + 1e-4,
                "right edge on screen at {w}x{h}: {rect:?}"
            );
            assert!(
                y + rh <= h as f32 + 1e-4,
                "bottom edge on screen at {w}x{h}: {rect:?}"
            );
            assert!(
                y + rh <= hotbar_top + 1e-4,
                "strip sits above the hotbar at {w}x{h}: {rect:?} vs {hotbar_top}"
            );
            assert!(
                (x + rw * 0.5 - w as f32 * 0.5).abs() < 1e-3,
                "strip is horizontally centered at {w}x{h}"
            );
        }
    }

    #[test]
    fn selected_name_rect_uses_the_hotbar_width_and_a_fixed_line_height() {
        let rects = hotbar_slot_rects(1280, 720);
        let first = rects[0];
        let last = rects[HOTBAR_SLOTS - 1];
        let rect = selected_name_rect(1280, 720);
        assert_close(rect.0, first.0, "left edge matches slot 0");
        assert_close(rect.2, (last.0 + last.2) - first.0, "spans the hotbar");
        assert_close(rect.3, SELECTED_NAME_HEIGHT, "one-line strip height");
        assert_close(
            rect.1 + rect.3,
            first.1 - SELECTED_NAME_GAP,
            "gap above the hotbar",
        );
    }

    #[test]
    fn selected_name_rect_stays_finite_on_degenerate_surfaces() {
        for (w, h) in [(0, 0), (1, 1), (8, 8)] {
            let rect = selected_name_rect(w, h);
            let (x, y, rw, rh) = rect;
            for (name, value) in [("x", x), ("y", y), ("w", rw), ("h", rh)] {
                assert!(value.is_finite(), "{name} finite on {w}x{h}");
            }
            assert!(x >= 0.0 && y >= 0.0, "starts on screen at {w}x{h}");
            assert!(
                x + rw <= w.max(1) as f32 + 1e-4,
                "right edge on screen at {w}x{h}"
            );
            assert!(
                y + rh <= h.max(1) as f32 + 1e-4,
                "bottom edge on screen at {w}x{h}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Damage flash quad (S06-T04)
    // -----------------------------------------------------------------------

    #[test]
    fn damage_flash_is_absent_for_quiescent_and_non_finite_intensities() {
        for intensity in [0.0, -0.5, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(
                damage_flash_quad(intensity, 1280, 720).is_none(),
                "intensity {intensity} must draw nothing"
            );
        }
    }

    #[test]
    fn damage_flash_covers_the_whole_surface_at_peak_alpha() {
        let quad = damage_flash_quad(1.0, 1280, 720).expect("a full hit draws the flash");
        assert_close(quad.x, 0.0, "flash x");
        assert_close(quad.y, 0.0, "flash y");
        assert_close(quad.w, 1280.0, "flash w covers the surface");
        assert_close(quad.h, 720.0, "flash h covers the surface");
        assert_close(quad.color[3], DAMAGE_FLASH_ALPHA, "peak alpha");
        assert_close(quad.color[3], 0.35, "pinned peak alpha");
        for channel in &quad.color[..3] {
            assert!(
                (0.0..=1.0).contains(channel),
                "flash channel {channel} out of range"
            );
        }
    }

    #[test]
    fn damage_flash_intensity_clamps_and_scales_alpha() {
        let over = damage_flash_quad(5.0, 640, 480).expect("5.0 still draws");
        assert_close(
            over.color[3],
            DAMAGE_FLASH_ALPHA,
            "above 1.0 clamps to peak alpha",
        );

        let half = damage_flash_quad(0.5, 640, 480).expect("0.5 draws");
        assert_close(half.color[3], 0.175, "half intensity is half alpha");
        assert_close(half.w, 640.0, "half intensity still covers the surface");
    }

    #[test]
    fn damage_flash_stays_finite_on_degenerate_surfaces() {
        let quad = damage_flash_quad(1.0, 0, 0).expect("a zero surface still yields a quad");
        for (name, value) in [("x", quad.x), ("y", quad.y), ("w", quad.w), ("h", quad.h)] {
            assert!(value.is_finite(), "{name} finite on 0x0");
        }
        assert_close(quad.w, 1.0, "max(1) keeps the width finite");
        assert_close(quad.h, 1.0, "max(1) keeps the height finite");
    }
}
