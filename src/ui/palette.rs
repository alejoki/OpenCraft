//! Pure searchable Creative palette: entry stock, filter, screen state,
//! pixel-space layout, hit testing, quads, and labels.
//!
//! Same purity contract as `src/ui/inventory.rs` and `src/ui/chest.rs`: no
//! winit, wgpu, `Player`, filesystem, or clock. The only outside values are the
//! frozen [`ContentRegistry`] (read when the palette opens) and data-only
//! player/UI types, so the whole 9x6 screen is provable headless before any
//! runtime input can move content.
//!
//! The palette is a separate screen rather than a third `InventoryMode`, which
//! keeps its state and draw path additive and leaves every pinned inventory
//! fixture untouched. It is opened by the documented key **C** in Creative
//! mode only (see `main.rs` and the README controls table); Escape closes it.
//! Entries are cached at open because the session registry is frozen
//! (MEM512/D181), and placeholders appended for missing pack content are
//! filtered out structurally.
//!
//! Coordinates are physical pixels, top-left origin, y down — the same space
//! as `Quad`, `ctx.config`, glyphon `TextArea`, and `GameApp.cursor_pos`
//! (P003/MEM012). `main.rs` owns the frame loop and rebuilds this layout every
//! frame, so a `Resized` reflows the screen automatically.

use crate::content::names;
use crate::content::ContentRegistry;
use crate::player::inventory::ItemId;
use crate::player::survival::GameMode;
use crate::ui::inventory::{
    InventoryLabel, Rect, LABEL_LINE_HEIGHT, PANEL_MARGIN, SLOT_GAP, SLOT_SIZE,
};
use crate::ui::text_input::{TextInput, TextInputAction, TextInputOutcome};
use crate::ui::Quad;

// ---------------------------------------------------------------------------
// Public constants
// ---------------------------------------------------------------------------

/// Palette grid width, in slots.
pub const PALETTE_COLUMNS: usize = 9;
/// Palette grid height, in slots.
pub const PALETTE_ROWS: usize = 6;
/// Slots visible at once (`PALETTE_COLUMNS * PALETTE_ROWS`).
pub const PALETTE_VISIBLE: usize = PALETTE_COLUMNS * PALETTE_ROWS;
/// Maximum number of text lines the palette may emit in one frame.
pub const MAX_PALETTE_LABELS: usize = 8;
/// Maximum search-query length, in characters.
pub const PALETTE_SEARCH_MAX_CHARS: usize = 32;

// ---------------------------------------------------------------------------
// Natural layout metrics (scaled to fit the surface)
// ---------------------------------------------------------------------------

const PANEL_PAD: f32 = 16.0;
const TITLE_HEIGHT: f32 = 24.0;
const TITLE_GAP: f32 = 8.0;
const SEARCH_HEIGHT: f32 = 24.0;
const SEARCH_GAP: f32 = 12.0;
const FOOTER_HEIGHT: f32 = 32.0;
const SEARCH_PAD_X: f32 = 8.0;

const GRID_W: f32 = PALETTE_COLUMNS as f32 * SLOT_SIZE + (PALETTE_COLUMNS as f32 - 1.0) * SLOT_GAP;
const GRID_H: f32 = PALETTE_ROWS as f32 * SLOT_SIZE + (PALETTE_ROWS as f32 - 1.0) * SLOT_GAP;
const SEARCH_W: f32 = GRID_W;
const PANEL_W: f32 = 2.0 * PANEL_PAD + GRID_W;
const SEARCH_TOP: f32 = PANEL_PAD + TITLE_HEIGHT + TITLE_GAP;
const GRID_TOP: f32 = SEARCH_TOP + SEARCH_HEIGHT + SEARCH_GAP;
const FOOTER_TOP: f32 = GRID_TOP + GRID_H + SEARCH_GAP;
const PANEL_H: f32 = FOOTER_TOP + FOOTER_HEIGHT + PANEL_PAD;

const PANEL_COLOR: [f32; 4] = [0.07, 0.07, 0.09, 0.94];
const SEARCH_COLOR: [f32; 4] = [0.13, 0.13, 0.17, 1.0];
const SLOT_COLOR: [f32; 4] = [0.20, 0.20, 0.24, 1.0];
const SELECTION_COLOR: [f32; 4] = [0.38, 0.34, 0.16, 1.0];
const HOVER_COLOR: [f32; 4] = [0.28, 0.30, 0.36, 1.0];

// ---------------------------------------------------------------------------
// Entry stock
// ---------------------------------------------------------------------------

/// One searchable palette row: the identity plus the labels it matches on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaletteEntry {
    /// Item identity granted by a click.
    pub id: ItemId,
    /// Stable short/pack id (`stone`, `mymod:ruby_ore`), never the display
    /// label.
    pub name: String,
    /// Authored human-readable label (`Stone`, `Ruby Ore`).
    pub display_name: String,
    /// Substring before `:` for namespaced ids, or `"builtin"` for unqualified
    /// built-in names.
    pub namespace: String,
}

/// Namespace reported for unqualified built-in content.
fn namespace_of(name: &str) -> String {
    name.split_once(':')
        .map_or("builtin", |(namespace, _)| namespace)
        .to_string()
}

fn push_entry(out: &mut Vec<PaletteEntry>, registry: &ContentRegistry, id: ItemId, name: String) {
    let display_name = registry
        .item_display_name(id)
        .unwrap_or_else(|| id.name().to_string());
    out.push(PaletteEntry {
        id,
        namespace: namespace_of(&name),
        name,
        display_name,
    });
}

/// Every non-placeholder block, material, tool, pack item, and pack tool the
/// registry knows, sorted by `(display_name.to_lowercase(), name)`.
///
/// Built-in blocks come from `registry.blocks()` in raw-id order and use the
/// frozen `BlockId::is_carryable` predicate (excludes Air, Bedrock, Water,
/// Farmland, the wheat stages, and the loot chest). Built-in materials and
/// tools follow the frozen `names` vocabulary order. Pack items/tools follow
/// their dense registry index. Placeholder definitions are filtered out before
/// sorting, so a missing-pack world can never list them.
pub fn entries(registry: &ContentRegistry) -> Vec<PaletteEntry> {
    let mut out = Vec::new();

    for def in registry.blocks() {
        if !def.placeholder && def.id.is_carryable() {
            push_entry(&mut out, registry, ItemId::Block(def.id), def.name.clone());
        }
    }

    for name in names::MATERIAL_ID_NAMES {
        if let Some(material) = names::builtin_material_by_name(name) {
            push_entry(
                &mut out,
                registry,
                ItemId::Material(material),
                names::builtin_material_id_name(material).to_string(),
            );
        }
    }

    for name in names::TOOL_ID_NAMES {
        if let Some(id) = names::builtin_tool_id_by_name(name) {
            if let Some((kind, tier)) = id.tool() {
                if let Some(short) = names::builtin_tool_id_name(kind, tier) {
                    push_entry(&mut out, registry, id, short.to_string());
                }
            }
        }
    }

    for def in registry.items() {
        if !def.placeholder {
            push_entry(
                &mut out,
                registry,
                ItemId::Item(def.index),
                def.name.clone(),
            );
        }
    }

    for def in registry.tools() {
        push_entry(
            &mut out,
            registry,
            ItemId::ModTool(def.index),
            def.name.clone(),
        );
    }

    out.sort_by(|left, right| {
        left.display_name
            .to_lowercase()
            .cmp(&right.display_name.to_lowercase())
            .then_with(|| left.name.cmp(&right.name))
    });
    out
}

/// Indices into `entries` matching `query`, in the entries' sorted order.
///
/// The query is trimmed; empty or whitespace-only input matches everything.
/// Matching is a case-insensitive substring test over the stable id, display
/// name, and namespace. There is deliberately no fuzzy matching or token
/// splitting, so the result is deterministic and pinnable.
pub fn filter_entries(entries: &[PaletteEntry], query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return (0..entries.len()).collect();
    }
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            entry.name.to_lowercase().contains(&needle)
                || entry.display_name.to_lowercase().contains(&needle)
                || entry.namespace.to_lowercase().contains(&needle)
        })
        .map(|(index, _)| index)
        .collect()
}

/// Whether the palette may open in `mode` (Creative only).
pub const fn available_in(mode: GameMode) -> bool {
    matches!(mode, GameMode::Creative)
}

// ---------------------------------------------------------------------------
// Screen state
// ---------------------------------------------------------------------------

/// Open state plus the cached, filtered entry list and selection.
pub struct PaletteScreen {
    open: bool,
    entries: Vec<PaletteEntry>,
    /// Cached indices into `entries`; rebuilt on open and query change.
    filtered: Vec<usize>,
    query: TextInput,
    /// Index into `filtered`, not `entries`.
    selected: usize,
    /// First visible index into `filtered`.
    scroll: usize,
    status: Option<String>,
}

impl PaletteScreen {
    /// Closed palette with an empty search field.
    pub fn new() -> Self {
        Self {
            open: false,
            entries: Vec::new(),
            filtered: Vec::new(),
            query: TextInput::new(PALETTE_SEARCH_MAX_CHARS),
            selected: 0,
            scroll: 0,
            status: None,
        }
    }

    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the palette against `registry`, resetting the query, selection,
    /// scroll, and status, and caching the sorted entry stock.
    pub fn open(&mut self, registry: &ContentRegistry) {
        self.open = true;
        self.entries = entries(registry);
        self.query = TextInput::new(PALETTE_SEARCH_MAX_CHARS);
        self.filtered = filter_entries(&self.entries, self.query.value());
        self.selected = 0;
        self.scroll = 0;
        self.status = None;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn entries(&self) -> &[PaletteEntry] {
        &self.entries
    }

    pub fn filtered(&self) -> &[usize] {
        &self.filtered
    }

    pub fn query(&self) -> &TextInput {
        &self.query
    }

    /// First visible filtered index (needed by the icon builder).
    pub const fn scroll(&self) -> usize {
        self.scroll
    }

    /// Applies one edit command. Returns true when the edit state changed (text
    /// or cursor), in which case the filtered list is rebuilt and the
    /// selection/scroll reset. Submit never rebuilds.
    pub fn apply(&mut self, action: TextInputAction) -> bool {
        match self.query.apply(action) {
            TextInputOutcome::Changed => {
                self.filtered = filter_entries(&self.entries, self.query.value());
                self.selected = 0;
                self.scroll = 0;
                true
            }
            TextInputOutcome::Unchanged | TextInputOutcome::Submitted(_) => false,
        }
    }

    /// Moves the selection by `delta`, clamped to the filtered range, and keeps
    /// it inside the visible window.
    pub fn move_selection(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            self.selected = 0;
            self.scroll = 0;
            return;
        }
        let max = self.filtered.len() - 1;
        let next = (self.selected as isize)
            .saturating_add(delta)
            .clamp(0, max as isize);
        self.selected = next as usize;
        self.ensure_visible();
    }

    /// Scrolls the visible window by `delta`, clamped to
    /// `0..=filtered.len().saturating_sub(PALETTE_VISIBLE)`.
    pub fn scroll_by(&mut self, delta: isize) {
        let max = self.filtered.len().saturating_sub(PALETTE_VISIBLE);
        let next = (self.scroll as isize)
            .saturating_add(delta)
            .clamp(0, max as isize);
        self.scroll = next as usize;
    }

    pub fn selected_entry(&self) -> Option<&PaletteEntry> {
        self.filtered
            .get(self.selected)
            .and_then(|index| self.entries.get(*index))
    }

    pub fn filtered_entry(&self, filtered_index: usize) -> Option<&PaletteEntry> {
        self.filtered
            .get(filtered_index)
            .and_then(|index| self.entries.get(*index))
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn set_status(&mut self, text: Option<String>) {
        self.status = text;
    }

    fn ensure_visible(&mut self) {
        if self.filtered.is_empty() {
            self.selected = 0;
            self.scroll = 0;
            return;
        }
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len() - 1;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + PALETTE_VISIBLE {
            self.scroll = self.selected + 1 - PALETTE_VISIBLE;
        }
        let max = self.filtered.len().saturating_sub(PALETTE_VISIBLE);
        self.scroll = self.scroll.min(max);
    }
}

impl Default for PaletteScreen {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// All pixel-space geometry for one palette frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaletteLayout {
    /// Dark backdrop panel.
    pub panel: Rect,
    /// Search-field background.
    pub search: Rect,
    /// All 54 visible slots in row-major order.
    pub slots: [Rect; PALETTE_VISIBLE],
}

/// Builds the layout for the given surface. Centers the panel and scales the
/// natural metrics down when the surface is smaller than the panel, keeping
/// every coordinate finite and every rect non-negative for 0x0 and 1x1
/// surfaces. The scale is capped at 1.0 exactly like the inventory/chest
/// screens (MEM506).
pub fn layout(surface_w: u32, surface_h: u32) -> PaletteLayout {
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
        let col = index % PALETTE_COLUMNS;
        let row = index / PALETTE_COLUMNS;
        to_px(
            PANEL_PAD + col as f32 * (SLOT_SIZE + SLOT_GAP),
            GRID_TOP + row as f32 * (SLOT_SIZE + SLOT_GAP),
            SLOT_SIZE,
            SLOT_SIZE,
        )
    });

    PaletteLayout {
        panel: to_px(0.0, 0.0, PANEL_W, PANEL_H),
        search: to_px(PANEL_PAD, SEARCH_TOP, SEARCH_W, SEARCH_HEIGHT),
        slots,
    }
}

// ---------------------------------------------------------------------------
// Hit testing
// ---------------------------------------------------------------------------

fn contains((x, y, w, h): Rect, (cx, cy): (f32, f32)) -> bool {
    cx >= x && cx < x + w && cy >= y && cy < y + h
}

/// Absolute filtered index under the cursor, or `None` when the palette is
/// closed, the cursor is outside every slot, or the slot is past the end of
/// the filtered list. Half-open containment matches `ui::inventory`/`ui::chest`.
pub fn hit_test(
    screen: &PaletteScreen,
    layout: &PaletteLayout,
    cursor: (f32, f32),
) -> Option<usize> {
    if !screen.is_open() {
        return None;
    }
    layout
        .slots
        .iter()
        .position(|rect| contains(*rect, cursor))
        .map(|visible| screen.scroll() + visible)
        .filter(|index| *index < screen.filtered().len())
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

/// All quads for one frame: panel, search-field background, all 54 slot fills,
/// the selection highlight, and the hover highlight. Empty for a closed screen.
///
/// Every slot is emitted even when the filtered list is shorter than the grid,
/// so the palette geometry is stable for the headless digest. Icons are drawn
/// by `ui::item_icon` after these quads and are not part of this pure list.
pub fn screen_quads(
    screen: &PaletteScreen,
    layout: &PaletteLayout,
    cursor: Option<(f32, f32)>,
) -> Vec<Quad> {
    if !screen.is_open() {
        return Vec::new();
    }

    let mut quads = Vec::with_capacity(2 + PALETTE_VISIBLE + 2);
    quads.push(rect_quad(layout.panel, PANEL_COLOR));
    quads.push(rect_quad(layout.search, SEARCH_COLOR));
    for rect in layout.slots.iter() {
        quads.push(rect_quad(*rect, SLOT_COLOR));
    }

    if !screen.filtered().is_empty() {
        // A scroll can move the selected row outside the window; clamp the
        // highlight to the viewport so the open-screen quad count stays
        // stable (exactly one selection highlight whenever results exist).
        let relative = screen
            .selected
            .saturating_sub(screen.scroll())
            .min(PALETTE_VISIBLE - 1);
        quads.push(rect_quad(layout.slots[relative], SELECTION_COLOR));
    }

    if let Some(cursor) = cursor {
        if cursor.0.is_finite() && cursor.1.is_finite() {
            if let Some(index) = hit_test(screen, layout, cursor) {
                if let Some(relative) = index.checked_sub(screen.scroll()) {
                    if relative < PALETTE_VISIBLE {
                        quads.push(rect_quad(layout.slots[relative], HOVER_COLOR));
                    }
                }
            }
        }
    }

    quads
}

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

/// All labels for one frame, positioned inside the panel/search/footer rects.
///
/// Emits exactly four lines while open: title, search value (or placeholder),
/// selected entry (or `no matches`), and footer status/hint. Empty for a
/// closed screen. The `MAX_PALETTE_LABELS` cap leaves headroom for future
/// chrome without ever becoming reachable by the screen today.
pub fn screen_labels(screen: &PaletteScreen, layout: &PaletteLayout) -> Vec<InventoryLabel> {
    let mut labels = Vec::new();
    if !screen.is_open() {
        return labels;
    }

    let pad_x = layout.panel.2 * (PANEL_PAD / PANEL_W);
    let pad_y = layout.panel.3 * (PANEL_PAD / PANEL_H);
    labels.push(InventoryLabel {
        text: "Creative Palette".to_string(),
        left: layout.panel.0 + pad_x,
        top: layout.panel.1 + pad_y,
    });

    let search_pad_y =
        layout.search.3 * (((SEARCH_HEIGHT - LABEL_LINE_HEIGHT) * 0.5) / SEARCH_HEIGHT);
    let search_pad_x = layout.search.2 * (SEARCH_PAD_X / SEARCH_W);
    labels.push(InventoryLabel {
        text: if screen.query().is_empty() {
            "Search...".to_string()
        } else {
            screen.query().value().to_string()
        },
        left: layout.search.0 + search_pad_x,
        top: layout.search.1 + search_pad_y,
    });

    let footer_top =
        layout.panel.1 + (PANEL_H - PANEL_PAD - FOOTER_HEIGHT) / PANEL_H * layout.panel.3;
    let footer_x = layout.panel.0 + pad_x;
    let selected_text = match screen.selected_entry() {
        Some(entry) => format!("{} ({})", entry.display_name, entry.name),
        None => "no matches".to_string(),
    };
    labels.push(InventoryLabel {
        text: selected_text,
        left: footer_x,
        top: footer_top,
    });

    let footer_text = match screen.status().filter(|text| !text.is_empty()) {
        Some(status) => status.to_string(),
        None => format!(
            "{}/{} items | Type to search | Click to add | Esc to close",
            screen.filtered().len(),
            screen.entries().len()
        ),
    };
    labels.push(InventoryLabel {
        text: footer_text,
        left: footer_x,
        top: footer_top + LABEL_LINE_HEIGHT,
    });

    debug_assert!(
        labels.len() <= MAX_PALETTE_LABELS,
        "palette screen emitted {} labels, exceeding MAX_PALETTE_LABELS={}",
        labels.len(),
        MAX_PALETTE_LABELS
    );
    labels
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::{build_registry, ContentRegistry, PackIdentity};
    use crate::player::inventory::Material;
    use std::path::Path;

    /// The checked-in reference pack through the same production mapping as
    /// `main.rs::build_session_registry`.
    fn reference_registry() -> ContentRegistry {
        let spec = crate::pack::spec::load_spec_from_dir(Path::new("packs/mymod-ruby"));
        let pack = (
            PackIdentity {
                folder: spec.folder.clone(),
                version: spec.version.clone(),
            },
            spec.content.clone(),
        );
        let (registry, report) = build_registry(ContentRegistry::builtin(), &[pack]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        registry
    }

    fn filtered_names(entries: &[PaletteEntry], query: &str) -> Vec<String> {
        filter_entries(entries, query)
            .into_iter()
            .map(|index| entries[index].name.clone())
            .collect()
    }

    /// A synthetic screen with `count` distinct entries, all filtered in, for
    /// scroll/clamp tests that need more than the 47-entry reference stock.
    fn synthetic_screen(count: usize) -> PaletteScreen {
        let mut screen = PaletteScreen::new();
        screen.open = true;
        screen.entries = (0..count)
            .map(|index| PaletteEntry {
                id: ItemId::Material(Material::Stick),
                name: format!("item_{index:03}"),
                display_name: format!("Item {index:03}"),
                namespace: "test".to_string(),
            })
            .collect();
        screen.filtered = (0..count).collect();
        screen
    }

    #[test]
    fn reference_registry_lists_47_entries_sorted_by_display_then_id() {
        let registry = reference_registry();
        let entries = entries(&registry);
        assert_eq!(entries.len(), 47);

        let mut builtin_blocks = 0;
        let mut pack_blocks = 0;
        let mut materials = 0;
        let mut builtin_tools = 0;
        let mut pack_items = 0;
        let mut pack_tools = 0;
        for entry in &entries {
            match entry.id {
                ItemId::Block(block) if block.is_builtin() => builtin_blocks += 1,
                ItemId::Block(_) => pack_blocks += 1,
                ItemId::Material(_) => materials += 1,
                ItemId::Tool { .. } => builtin_tools += 1,
                ItemId::Item(_) => pack_items += 1,
                ItemId::ModTool(_) => pack_tools += 1,
            }
        }
        assert_eq!(builtin_blocks, 14);
        assert_eq!(pack_blocks, 2);
        assert_eq!(materials, 11);
        assert_eq!(builtin_tools, 16);
        assert_eq!(pack_items, 2);
        assert_eq!(pack_tools, 2);

        for pair in entries.windows(2) {
            let left = (pair[0].display_name.to_lowercase(), pair[0].name.as_str());
            let right = (pair[1].display_name.to_lowercase(), pair[1].name.as_str());
            assert!(left <= right, "entries out of order: {left:?} > {right:?}");
        }

        let ruby = entries
            .iter()
            .find(|entry| entry.name == "mymod:ruby")
            .expect("reference pack ruby item");
        assert_eq!(ruby.display_name, "Ruby");
        assert_eq!(ruby.namespace, "mymod");

        let stone = entries
            .iter()
            .find(|entry| entry.name == "stone")
            .expect("built-in stone block");
        assert_eq!(stone.namespace, "builtin");
    }

    #[test]
    fn filter_is_case_insensitive_and_matches_name_display_and_namespace() {
        let registry = reference_registry();
        let entries = entries(&registry);

        assert_eq!(
            filtered_names(&entries, "rub"),
            vec![
                "mymod:ruby".to_string(),
                "mymod:ruby_block".to_string(),
                "mymod:ruby_ore".to_string(),
                "mymod:ruby_pickaxe".to_string(),
                "mymod:ruby_stew".to_string(),
                "mymod:ruby_sword".to_string(),
            ]
        );
        assert_eq!(filtered_names(&entries, " RUB ").len(), 6);
        assert_eq!(filtered_names(&entries, "mymod").len(), 6);
        assert_eq!(filtered_names(&entries, "builtin").len(), 41);
        assert_eq!(filtered_names(&entries, "Ruby Ore"), vec!["mymod:ruby_ore"]);
        assert_eq!(
            filtered_names(&entries, "wood_pickaxe"),
            vec!["wood_pickaxe"]
        );
        assert_eq!(filtered_names(&entries, "").len(), 47);
        assert_eq!(filtered_names(&entries, "   ").len(), 47);
        assert!(filtered_names(&entries, "no-such-content").is_empty());
    }

    #[test]
    fn placeholder_definitions_are_never_listed() {
        let registry = reference_registry();
        let mut with_placeholders = registry.clone();
        let ghost_block = with_placeholders.append_placeholder_block("ghost_ore");
        let ghost_item = with_placeholders.append_placeholder_item("ghost_gem");
        assert!(matches!(
            ghost_block,
            crate::world::block::BlockId::Modded(_)
        ));
        assert!(
            with_placeholders
                .block(ghost_block)
                .expect("appended placeholder block")
                .placeholder
        );
        assert!(
            with_placeholders
                .item(ghost_item)
                .expect("appended placeholder item")
                .placeholder
        );

        let entries = entries(&with_placeholders);
        assert_eq!(entries.len(), 47);
        assert!(entries.iter().all(|entry| entry.name != "ghost_ore"));
        assert!(entries
            .iter()
            .all(|entry| !entry.name.contains("ghost_gem")));
        assert!(filtered_names(&entries, "ghost").is_empty());
        assert!(filtered_names(&entries, "placeholder").is_empty());
    }

    #[test]
    fn selection_and_scroll_clamp() {
        let registry = reference_registry();
        let mut screen = PaletteScreen::new();
        screen.open(&registry);

        screen.move_selection(-100);
        assert_eq!(screen.selected, 0);
        screen.move_selection(10_000);
        assert_eq!(screen.selected, 46);
        assert_eq!(screen.scroll(), 0);
        screen.scroll_by(-10);
        assert_eq!(screen.scroll(), 0);
        screen.scroll_by(1_000);
        assert_eq!(screen.scroll(), 0);

        let mut screen = synthetic_screen(100);
        screen.move_selection(60);
        assert_eq!(screen.selected, 60);
        assert_eq!(screen.scroll(), 7);
        screen.scroll_by(1_000);
        assert_eq!(screen.scroll(), 46);
        screen.scroll_by(-1_000);
        assert_eq!(screen.scroll(), 0);
        screen.move_selection(-1_000);
        assert_eq!(screen.selected, 0);
        assert_eq!(screen.scroll(), 0);
        screen.move_selection(10_000);
        assert_eq!(screen.selected, 99);
        assert_eq!(screen.scroll(), 46);
    }

    #[test]
    fn apply_rebuilds_filter_on_changed_outcome() {
        let registry = reference_registry();
        let mut screen = PaletteScreen::new();
        screen.open(&registry);
        assert_eq!(screen.filtered().len(), 47);
        assert_eq!(
            screen.selected_entry().map(|entry| entry.name.as_str()),
            Some("apple")
        );

        assert!(screen.apply(TextInputAction::InsertText("rub".to_string())));
        assert_eq!(screen.query().value(), "rub");
        assert_eq!(screen.filtered().len(), 6);
        assert_eq!(
            screen.selected_entry().map(|entry| entry.name.as_str()),
            Some("mymod:ruby")
        );
        assert_eq!(screen.scroll(), 0);

        screen.move_selection(2);
        assert_eq!(
            screen.selected_entry().map(|entry| entry.name.as_str()),
            Some("mymod:ruby_ore")
        );
        assert!(!screen.apply(TextInputAction::Submit));
        assert_eq!(screen.filtered().len(), 6);
        assert_eq!(
            screen.selected_entry().map(|entry| entry.name.as_str()),
            Some("mymod:ruby_ore")
        );

        assert!(screen.apply(TextInputAction::Clear));
        assert_eq!(screen.filtered().len(), 47);
        assert_eq!(screen.selected, 0);
        assert_eq!(screen.scroll(), 0);
        // Cursor movement reports `Changed`, so it also resets selection; a
        // move at the home position is a genuine no-op.
        assert!(!screen.apply(TextInputAction::MoveLeft));
    }

    #[test]
    fn layout_pins_palette_geometry_at_720p() {
        let layout = layout(1280, 720);
        assert_eq!(layout.panel, (428.0, 158.0, 424.0, 404.0));
        assert_eq!(layout.search, (444.0, 206.0, 392.0, 24.0));
        assert_eq!(layout.slots[0], (444.0, 242.0, 40.0, 40.0));
        assert_eq!(layout.slots[53], (796.0, 462.0, 40.0, 40.0));

        let panel_right = layout.panel.0 + layout.panel.2;
        let panel_bottom = layout.panel.1 + layout.panel.3;
        for (index, slot) in layout.slots.iter().enumerate() {
            assert!(slot.0 >= layout.panel.0 && slot.1 >= layout.panel.1);
            assert!(
                slot.0 + slot.2 <= panel_right && slot.1 + slot.3 <= panel_bottom,
                "slot {index} escapes the panel: {slot:?}"
            );
        }
        for (index, slot) in layout.slots.iter().enumerate() {
            for other in layout.slots.iter().skip(index + 1) {
                assert_ne!(
                    slot, other,
                    "slots {index} and a later slot overlap exactly"
                );
            }
        }
    }

    #[test]
    fn layout_is_finite_for_degenerate_surfaces() {
        for (width, height) in [(0, 0), (1, 1), (320, 200)] {
            let layout = layout(width, height);
            let mut rects = vec![layout.panel, layout.search];
            rects.extend(layout.slots.iter().copied());
            for rect in rects {
                assert!(
                    rect.0.is_finite()
                        && rect.1.is_finite()
                        && rect.2.is_finite()
                        && rect.3.is_finite(),
                    "non-finite rect {rect:?} for {width}x{height}"
                );
                assert!(rect.2 >= 0.0 && rect.3 >= 0.0);
            }
        }
    }

    #[test]
    fn hit_test_returns_absolute_filtered_index() {
        let registry = reference_registry();
        let layout = layout(1280, 720);
        let mut screen = PaletteScreen::new();
        screen.open(&registry);

        let slot0 = layout.slots[0];
        let center0 = (slot0.0 + slot0.2 * 0.5, slot0.1 + slot0.3 * 0.5);
        assert_eq!(hit_test(&screen, &layout, center0), Some(0));

        screen.close();
        assert_eq!(hit_test(&screen, &layout, center0), None);
        screen.open(&registry);
        assert_eq!(hit_test(&screen, &layout, (0.0, 0.0)), None);

        let mut screen = synthetic_screen(100);
        screen.scroll_by(10);
        assert_eq!(hit_test(&screen, &layout, center0), Some(10));

        screen.filtered.truncate(5);
        assert_eq!(hit_test(&screen, &layout, center0), None);
    }

    #[test]
    fn selection_highlight_survives_scrolling_the_selection_away() {
        let layout = layout(1280, 720);
        let mut screen = synthetic_screen(100);
        screen.scroll_by(10);
        assert_eq!(screen.selected, 0);
        assert_eq!(screen.scroll(), 10);
        assert_eq!(screen_quads(&screen, &layout, None).len(), 57);
    }

    #[test]
    fn open_screen_draws_57_quads_and_4_labels() {
        let registry = reference_registry();
        let mut screen = PaletteScreen::new();
        screen.open(&registry);
        let layout = layout(1280, 720);

        let quads = screen_quads(&screen, &layout, None);
        assert_eq!(quads.len(), 57);
        let labels = screen_labels(&screen, &layout);
        assert_eq!(labels.len(), 4);
        assert_eq!(labels[0].text, "Creative Palette");
        assert_eq!(labels[1].text, "Search...");
        assert_eq!(labels[2].text, "Apple (apple)");
        assert!(labels[3].text.contains("47/47 items"));
    }

    #[test]
    fn closed_screen_draws_nothing() {
        let layout = layout(1280, 720);
        let screen = PaletteScreen::new();
        assert!(screen_quads(&screen, &layout, Some((500.0, 300.0))).is_empty());
        assert!(screen_labels(&screen, &layout).is_empty());
    }

    #[test]
    fn empty_filter_still_draws_grid_and_no_matches() {
        let registry = reference_registry();
        let mut screen = PaletteScreen::new();
        screen.open(&registry);
        assert!(screen.apply(TextInputAction::InsertText("zzzz".to_string())));
        assert!(screen.filtered().is_empty());

        let layout = layout(1280, 720);
        assert_eq!(screen_quads(&screen, &layout, None).len(), 56);
        let labels = screen_labels(&screen, &layout);
        assert_eq!(labels.len(), 4);
        assert_eq!(labels[2].text, "no matches");
        assert!(labels[3].text.contains("0/47 items"));
    }

    #[test]
    fn available_in_is_creative_only() {
        assert!(available_in(GameMode::Creative));
        assert!(!available_in(GameMode::Survival));
    }
}
