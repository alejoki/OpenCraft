//! Headless digest of the pure inventory/HUD surfaces.
//!
//! `scripts/ui_smoke.sh` (and the eventual `--demo-ui` flag) need one
//! deterministic line per resolution that proves the exact same pure functions
//! the frame loop uses: textured block/sprite icon counts, panel layout and the
//! hotbar separator, crafting-table art layers, tooltip flip/inside geometry,
//! selected-name fade, and vitals spacing. No winit, wgpu, filesystem, clock,
//! or `Player` is touched here, so the digest runs on any machine.
//!
//! Every line names the failing checks in `failed=` rather than printing an
//! unconditional success line: a regression flips `ok=false`, and the smoke
//! fails closed instead of blessing broken UI.

use crate::pack::AUTHORABLE_TEX_LAYERS;
use crate::player::inventory::{
    CraftingGrid, GridKind, Inventory, Item, ItemId, Material, ToolKind, ToolTier, HOTBAR_SLOTS,
    INVENTORY_SLOTS,
};
use crate::player::survival::{GameMode, SurvivalState};
use crate::render::texture::generate_all_texture_layers_rgba;
use crate::ui::hud::{
    hotbar_count_label_for, hotbar_icon_rect, hotbar_slot_rects, vitals_metrics, vitals_quads,
    AIR_FILL_COLOR, HEART_FILL_COLOR, HUNGER_FILL_COLOR,
};
use crate::ui::inventory::{layout, screen_quads, tooltip_lines, tooltip_rect, InventoryMode};
use crate::ui::item_icon::{hotbar_icon_vertices, inventory_icon_vertices};
use crate::ui::selection::{SelectionLabel, SELECTED_NAME_SECS};
use crate::world::block::BlockId;

/// Resolutions the digest pins, in the same order as the smoke script.
const DIGEST_SIZES: [(u32, u32); 3] = [(1280, 720), (1920, 1080), (3840, 2160)];

/// One procedural texture layer is 16x16 RGBA.
const TEXTURE_LAYER_BYTES: usize = 16 * 16 * 4;

/// Tooltip text width used to force a right/bottom flip at the bottom-right
/// anchor. Wide enough that the naive placement overflows every pinned size.
const TOOLTIP_TEXT_WIDTH: f32 = 120.0;

/// The deterministic fixture: populated inventory slots, both crafting grids,
/// and the cursor stack.
struct DemoFixture {
    inventory: Inventory,
    player_grid: CraftingGrid,
    table_grid: CraftingGrid,
}

/// Crafting-table art facts, computed once per digest run.
struct TableArt {
    top: u8,
    side: u8,
    bottom: u8,
    authorable: usize,
    distinct: bool,
}

fn block(id: BlockId, count: u16) -> Item {
    Item::new(ItemId::Block(id), count).expect("fixture block is carryable")
}

fn material(id: Material, count: u16) -> Item {
    Item::new(ItemId::Material(id), count).expect("fixture material stack is valid")
}

fn tool(kind: ToolKind, tier: ToolTier) -> Item {
    Item::new(ItemId::Tool { kind, tier }, 1).expect("fixture tool is constructible")
}

fn tool_with(kind: ToolKind, tier: ToolTier, durability: u16) -> Item {
    Item::tool_with_durability(kind, tier, durability).expect("fixture durability is valid")
}

fn iron_pickaxe(durability: u16) -> Item {
    tool_with(ToolKind::Pickaxe, ToolTier::Iron, durability)
}

/// Build the pinned fixture: eleven occupied slots (three blocks followed by
/// eight sprites), empty storage, two crafting grids that both preview the
/// crafting table, and a bone cursor.
fn fixture() -> DemoFixture {
    let mut slots: [Option<Item>; INVENTORY_SLOTS] = [None; INVENTORY_SLOTS];
    let occupied = [
        block(BlockId::Grass, 64),
        block(BlockId::OakLog, 16),
        block(BlockId::CraftingTable, 1),
        iron_pickaxe(249),
        material(Material::Apple, 3),
        material(Material::Stick, 12),
        material(Material::Bread, 2),
        material(Material::Bone, 5),
        material(Material::Leather, 7),
        tool(ToolKind::Pickaxe, ToolTier::Stone),
        tool(ToolKind::Axe, ToolTier::Wood),
    ];
    for (index, item) in occupied.into_iter().enumerate() {
        slots[index] = Some(item);
    }

    // Both grids craft the table; the player grid fills its whole 2x2 and the
    // table grid occupies the top-left 2x2 of the 4x4.
    let mut player_grid = CraftingGrid::new(GridKind::Player);
    for index in 0..player_grid.kind().slot_count() {
        player_grid
            .set(index, Some(block(BlockId::OakPlanks, 1)))
            .expect("player cell is active");
    }
    let mut table_grid = CraftingGrid::new(GridKind::Table);
    for index in [0usize, 1, 4, 5] {
        table_grid
            .set(index, Some(block(BlockId::OakPlanks, 1)))
            .expect("table cell is active");
    }

    DemoFixture {
        inventory: Inventory::from_parts(slots, Some(material(Material::Bone, 2))),
        player_grid,
        table_grid,
    }
}

/// Crafting-table face layers plus the distinctness of the three procedural
/// layers, so a pack/art regression shows up as `table_distinct=0`.
fn table_art() -> TableArt {
    let top = BlockId::CraftingTable.face_tex_layer(2);
    let side = BlockId::CraftingTable.face_tex_layer(0);
    let bottom = BlockId::CraftingTable.face_tex_layer(3);
    let layers = generate_all_texture_layers_rgba();
    let layer = |index: u8| layers.get(index as usize).map(Vec::as_slice);
    let distinct = match (layer(top), layer(side), layer(bottom)) {
        (Some(top_bytes), Some(side_bytes), Some(bottom_bytes)) => {
            let shaped = |bytes: &[u8]| bytes.len() == TEXTURE_LAYER_BYTES;
            shaped(top_bytes)
                && shaped(side_bytes)
                && shaped(bottom_bytes)
                && top_bytes != side_bytes
                && top_bytes != bottom_bytes
                && side_bytes != bottom_bytes
        }
        _ => false,
    };
    TableArt {
        top,
        side,
        bottom,
        authorable: AUTHORABLE_TEX_LAYERS,
        distinct,
    }
}

fn push_check(failures: &mut Vec<&'static str>, name: &'static str, passed: bool) {
    if !passed {
        failures.push(name);
    }
}

/// `none` for a clean line, otherwise the comma-separated failing check names.
fn format_failures(failures: &[&'static str]) -> String {
    if failures.is_empty() {
        "none".to_string()
    } else {
        failures.join(",")
    }
}

fn status_suffix(ok: bool) -> &'static str {
    if ok {
        "ok=true"
    } else {
        "ok=false"
    }
}

fn finite_rect_inside(rect: (f32, f32, f32, f32), width: u32, height: u32) -> bool {
    let (x, y, w, h) = rect;
    let width = width as f32;
    let height = height as f32;
    [x, y, w, h].iter().all(|value| value.is_finite())
        && x >= 0.0
        && y >= 0.0
        && x + w <= width
        && y + h <= height
}

fn overflow_flag(flag: bool) -> usize {
    usize::from(flag)
}

/// Build one digest line and report whether every check on it passed.
fn digest_line(
    surface_w: u32,
    surface_h: u32,
    fixture: &DemoFixture,
    art: &TableArt,
) -> (String, bool) {
    let cursor_item = fixture.inventory.cursor();
    let cursor_pos = Some((surface_w as f32 * 0.5, surface_h as f32 * 0.5));

    let player_layout = layout(surface_w, surface_h, InventoryMode::Player);
    let table_layout = layout(surface_w, surface_h, InventoryMode::Table);

    let player_icons = inventory_icon_vertices(
        InventoryMode::Player,
        &player_layout,
        &fixture.inventory,
        &fixture.player_grid,
        &fixture.table_grid,
        cursor_item,
        cursor_pos,
        surface_w,
        surface_h,
    );
    let table_icons = inventory_icon_vertices(
        InventoryMode::Table,
        &table_layout,
        &fixture.inventory,
        &fixture.player_grid,
        &fixture.table_grid,
        cursor_item,
        cursor_pos,
        surface_w,
        surface_h,
    );
    let hotbar_icons = hotbar_icon_vertices(&fixture.inventory, surface_w, surface_h);

    let player_quads = screen_quads(
        InventoryMode::Player,
        &player_layout,
        &fixture.inventory,
        &fixture.player_grid,
        &fixture.table_grid,
        cursor_item,
        cursor_pos,
    );
    let table_quads = screen_quads(
        InventoryMode::Table,
        &table_layout,
        &fixture.inventory,
        &fixture.player_grid,
        &fixture.table_grid,
        cursor_item,
        cursor_pos,
    );

    let empty = fixture
        .inventory
        .slots()
        .iter()
        .filter(|slot| slot.is_none())
        .count();
    let empty_table = (0..fixture.table_grid.kind().slot_count())
        .filter(|index| fixture.table_grid.slot(*index).is_none())
        .count();

    let last_slot = player_layout.slots[INVENTORY_SLOTS - 1];
    let sep_gap = player_layout.slots[0].1 - (last_slot.1 + last_slot.3);
    let sep_h = player_layout.hotbar_separator.3;
    let (panel_w, panel_h) = (player_layout.panel.2, player_layout.panel.3);

    // Tooltip: two lines (name + durability), anchored bottom-right with a wide
    // text run so the naive right/below placement would overflow.
    let pickaxe = iron_pickaxe(249);
    let tooltip = tooltip_lines(pickaxe);
    let anchor = (surface_w as f32 - 1.0, surface_h as f32 - 1.0);
    let tooltip_box = tooltip_rect(
        anchor,
        TOOLTIP_TEXT_WIDTH,
        tooltip.len(),
        surface_w,
        surface_h,
    );
    let tooltip_flip = tooltip_box.0 < anchor.0 && tooltip_box.1 < anchor.1;
    let tooltip_inside = finite_rect_inside(tooltip_box, surface_w, surface_h);

    // Selected-name fade: visible at full alpha, fades to zero, restarts on a
    // durability change, and stays hidden for an empty slot.
    let mut selection = SelectionLabel::new();
    selection.observe(3, Some(pickaxe));
    let select_alpha0 = selection.alpha();
    selection.tick(SELECTED_NAME_SECS);
    let select_alpha2 = selection.alpha();
    let select_restart = selection.observe(3, Some(iron_pickaxe(248)));
    let restart_alpha = selection.alpha();
    selection.observe(4, None);
    let empty_hidden = !selection.is_visible() && selection.label().is_none();

    // Vitals: full survival, submerged, so all three rows exist. Colors are
    // the only way to tell hearts, hunger, and air apart after the pure builder
    // has flattened them into one draw list.
    let vitals = vitals_quads(
        &SurvivalState::full(),
        GameMode::Survival,
        true,
        surface_w,
        surface_h,
    );
    let metrics = vitals_metrics(surface_w, surface_h);
    let hearts: Vec<_> = vitals
        .iter()
        .filter(|quad| quad.color == HEART_FILL_COLOR)
        .collect();
    let hunger: Vec<_> = vitals
        .iter()
        .filter(|quad| quad.color == HUNGER_FILL_COLOR)
        .collect();
    let air: Vec<_> = vitals
        .iter()
        .filter(|quad| quad.color == AIR_FILL_COLOR)
        .collect();
    let hearts_right = hearts
        .iter()
        .map(|quad| quad.x + quad.w)
        .fold(f32::NEG_INFINITY, f32::max);
    let hunger_left = hunger
        .iter()
        .map(|quad| quad.x)
        .fold(f32::INFINITY, f32::min);
    let vitals_spaced = hearts.len() == 10
        && hunger.len() == 10
        && hunger_left - hearts_right >= metrics.icon_gap - 1e-3;
    let vitals_overlap = overflow_flag(!vitals_spaced);
    let hunger_top = hunger
        .iter()
        .map(|quad| quad.y)
        .fold(f32::INFINITY, f32::min);
    let air_bottom = air
        .iter()
        .map(|quad| quad.y + quad.h)
        .fold(f32::NEG_INFINITY, f32::max);
    let air_line = air.len() == 10 && air_bottom < hunger_top;

    let rects = hotbar_slot_rects(surface_w, surface_h);
    // Counts are now an overlay: the icon owns the whole slot, and a count of
    // one draws no label at all. The render-side shadow/order is pinned by the
    // main-binary source test; this digest proves the pure geometry contract.
    let count_clear = rects.iter().all(|rect| hotbar_icon_rect(*rect) == *rect)
        && (0..HOTBAR_SLOTS).all(|slot| match fixture.inventory.hotbar(slot) {
            Some(item) if item.count() <= 1 => {
                hotbar_count_label_for(&fixture.inventory, slot).is_empty()
            }
            _ => true,
        });

    let mut failures = Vec::new();
    push_check(
        &mut failures,
        "player_block",
        player_icons.block.len() == 144,
    );
    push_check(
        &mut failures,
        "player_sprite",
        player_icons.sprite.len() == 54,
    );
    push_check(&mut failures, "table_block", table_icons.block.len() == 234);
    push_check(
        &mut failures,
        "table_sprite",
        table_icons.sprite.len() == 54,
    );
    push_check(
        &mut failures,
        "hotbar_block",
        hotbar_icons.block.len() == 54,
    );
    push_check(
        &mut failures,
        "hotbar_sprite",
        hotbar_icons.sprite.len() == 36,
    );
    push_check(&mut failures, "empty", empty == 25);
    push_check(&mut failures, "empty_table", empty_table == 12);
    push_check(&mut failures, "quads_player", player_quads.len() == 60);
    push_check(&mut failures, "quads_table", table_quads.len() == 82);
    push_check(&mut failures, "sep_gap", (sep_gap - 16.0).abs() < 1e-4);
    push_check(&mut failures, "sep_h", (sep_h - 2.0).abs() < 1e-4);
    push_check(
        &mut failures,
        "panel",
        (panel_w - 676.0).abs() < 1e-4 && (panel_h - 384.0).abs() < 1e-4,
    );
    push_check(
        &mut failures,
        "table",
        art.top == 23 && art.side == 24 && art.bottom == 9,
    );
    push_check(&mut failures, "authorable", art.authorable == 25);
    push_check(&mut failures, "table_distinct", art.distinct);
    push_check(&mut failures, "tooltip_lines", tooltip.len() == 2);
    push_check(&mut failures, "tooltip_flip", tooltip_flip);
    push_check(&mut failures, "tooltip_inside", tooltip_inside);
    push_check(
        &mut failures,
        "select_alpha0",
        (select_alpha0 - 1.0).abs() < 1e-6,
    );
    push_check(&mut failures, "select_alpha2", select_alpha2.abs() < 1e-6);
    push_check(
        &mut failures,
        "select_restart",
        select_restart && (restart_alpha - 1.0).abs() < 1e-6,
    );
    push_check(&mut failures, "empty_hidden", empty_hidden);
    push_check(
        &mut failures,
        "vitals_icon",
        metrics.icon_size.is_finite()
            && metrics.icon_size > 0.0
            && metrics.row_width > metrics.icon_size
            && metrics.icon_gap > 0.0,
    );
    push_check(&mut failures, "vitals_overlap", vitals_overlap == 0);
    push_check(&mut failures, "air_line", air_line);
    push_check(&mut failures, "count_clear", count_clear);

    let ok = failures.is_empty();
    let line = format!(
        "ui_demo: size={surface_w}x{surface_h} player_block={} player_sprite={} \
         table_block={} table_sprite={} hotbar_block={} hotbar_sprite={} empty={} \
         empty_table={} quads_player={} quads_table={} sep_gap={:.3} sep_h={:.3} \
         panel={:.3}x{:.3} table={}/{}/{} authorable={} table_distinct={} \
         tooltip_lines={} tooltip_flip={} tooltip_inside={} select_alpha0={:.3} \
         select_alpha2={:.3} select_restart={} empty_hidden={} vitals_icon={:.3} \
         vitals_row={:.3} vitals_gap={:.3} vitals_overlap={} air_line={} count_clear={} \
         failed={} {}",
        player_icons.block.len(),
        player_icons.sprite.len(),
        table_icons.block.len(),
        table_icons.sprite.len(),
        hotbar_icons.block.len(),
        hotbar_icons.sprite.len(),
        empty,
        empty_table,
        player_quads.len(),
        table_quads.len(),
        sep_gap,
        sep_h,
        panel_w,
        panel_h,
        art.top,
        art.side,
        art.bottom,
        art.authorable,
        usize::from(art.distinct),
        tooltip.len(),
        usize::from(tooltip_flip),
        usize::from(tooltip_inside),
        select_alpha0,
        select_alpha2,
        usize::from(select_restart),
        usize::from(empty_hidden),
        metrics.icon_size,
        metrics.row_width,
        metrics.icon_gap,
        vitals_overlap,
        usize::from(air_line),
        usize::from(count_clear),
        format_failures(&failures),
        status_suffix(ok),
    );
    (line, ok)
}

/// Three pinned digest lines (1280x720, 1920x1080, 3840x2160) plus a flag that
/// is true only when every check passed at every resolution.
///
/// The line text is mirrored verbatim in `scripts/ui_smoke.sh`; update both
/// together when the fixture or an expected count changes.
pub fn ui_demo_digest_lines() -> (Vec<String>, bool) {
    let fixture = fixture();
    let art = table_art();
    let mut ok = true;
    let mut lines = Vec::with_capacity(DIGEST_SIZES.len());
    for (surface_w, surface_h) in DIGEST_SIZES {
        let (line, line_ok) = digest_line(surface_w, surface_h, &fixture, &art);
        ok &= line_ok;
        lines.push(line);
    }
    (lines, ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::item_icon::IconSource;

    // Pinned in two places on purpose: `scripts/ui_smoke.sh` mirrors these
    // exact lines. Update both halves together when a fixture or count changes.
    const PINNED_LINES: [&str; 3] = [
        "ui_demo: size=1280x720 player_block=144 player_sprite=54 table_block=234 table_sprite=54 hotbar_block=54 hotbar_sprite=36 empty=25 empty_table=12 quads_player=60 quads_table=82 sep_gap=16.000 sep_h=2.000 panel=676.000x384.000 table=23/24/9 authorable=25 table_distinct=1 tooltip_lines=2 tooltip_flip=1 tooltip_inside=1 select_alpha0=1.000 select_alpha2=0.000 select_restart=1 empty_hidden=1 vitals_icon=14.577 vitals_row=162.169 vitals_gap=1.822 vitals_overlap=0 air_line=1 count_clear=1 failed=none ok=true",
        "ui_demo: size=1920x1080 player_block=144 player_sprite=54 table_block=234 table_sprite=54 hotbar_block=54 hotbar_sprite=36 empty=25 empty_table=12 quads_player=60 quads_table=82 sep_gap=16.000 sep_h=2.000 panel=676.000x384.000 table=23/24/9 authorable=25 table_distinct=1 tooltip_lines=2 tooltip_flip=1 tooltip_inside=1 select_alpha0=1.000 select_alpha2=0.000 select_restart=1 empty_hidden=1 vitals_icon=21.865 vitals_row=243.253 vitals_gap=2.733 vitals_overlap=0 air_line=1 count_clear=1 failed=none ok=true",
        "ui_demo: size=3840x2160 player_block=144 player_sprite=54 table_block=234 table_sprite=54 hotbar_block=54 hotbar_sprite=36 empty=25 empty_table=12 quads_player=60 quads_table=82 sep_gap=16.000 sep_h=2.000 panel=676.000x384.000 table=23/24/9 authorable=25 table_distinct=1 tooltip_lines=2 tooltip_flip=1 tooltip_inside=1 select_alpha0=1.000 select_alpha2=0.000 select_restart=1 empty_hidden=1 vitals_icon=43.731 vitals_row=486.507 vitals_gap=5.466 vitals_overlap=0 air_line=1 count_clear=1 failed=none ok=true",
    ];

    #[test]
    fn digest_lines_are_pinned() {
        let (lines, ok) = ui_demo_digest_lines();
        assert!(ok, "digest reported a failed check: {lines:?}");
        assert_eq!(lines, PINNED_LINES);
    }

    #[test]
    fn digest_lines_are_deterministic() {
        assert_eq!(ui_demo_digest_lines(), ui_demo_digest_lines());
    }

    #[test]
    fn failure_field_formats_named_failures() {
        assert_eq!(format_failures(&[]), "none");
        assert_eq!(format_failures(&["a", "b"]), "a,b");
        assert_eq!(status_suffix(true), "ok=true");
        assert_eq!(status_suffix(false), "ok=false");
    }

    #[test]
    fn fixture_covers_expected_slots() {
        let fixture = fixture();
        let occupied = fixture
            .inventory
            .slots()
            .iter()
            .filter(|slot| slot.is_some())
            .count();
        let empty = fixture
            .inventory
            .slots()
            .iter()
            .filter(|slot| slot.is_none())
            .count();
        assert_eq!(occupied, 11);
        assert_eq!(empty, 25);

        let mut blocks = 0;
        let mut sprites = 0;
        for slot in fixture.inventory.slots().iter().flatten() {
            match crate::ui::item_icon::icon_source(slot.id()) {
                Some(IconSource::Block(_)) => blocks += 1,
                Some(IconSource::Sprite(_)) => sprites += 1,
                None => panic!("fixture item has no icon source: {:?}", slot.id()),
            }
        }
        assert_eq!((blocks, sprites), (3, 8));
    }
}
