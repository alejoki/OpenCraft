//! Pure block-item cube icon geometry and texture-array face mapping.
//!
//! Everything above the renderer (added in a later task) is GPU-free: given a
//! pixel-space slot rect it emits textured triangles in clip space, exactly
//! like `ui::quad` and `ui::logo` do for their own vertices. Keeping this
//! module's geometry pure means the three-face cube can be proven headless,
//! before any wgpu device exists.
//!
//! Coordinates are physical pixels, top-left origin, y down, converted to NDC
//! with the codebase-wide convention (`x_ndc = 2x/sw - 1`,
//! `y_ndc = 1 - 2y/sh`, surface dimensions guarded with `.max(1)`).

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use super::hud::{hotbar_icon_rect, hotbar_slot_rects};
use super::inventory::{InventoryLayout, InventoryMode, Rect};
use crate::player::inventory::{
    CraftingGrid, Inventory, Item, ItemId, Material, ToolKind, ToolTier,
};
use crate::render::context::RenderContext;
use crate::world::block::BlockId;

/// Fraction of the icon's smaller edge kept as empty padding on each side.
pub const ICON_INSET_FRACTION: f32 = 0.15;

/// One textured UI icon vertex: clip-space position, texture UV, and the
/// `texture_2d_array` layer to sample.
///
/// The trailing `_pad` keeps the struct free of implicit padding while still
/// matching the 32-byte stride a `Float32x4 + Float32x2 + Uint32` GPU layout
/// expects (offsets 0, 16, 24; stride 32). It is always zero.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct IconVertex {
    pub clip_pos: [f32; 4],
    pub uv: [f32; 2],
    pub layer: u32,
    _pad: [u8; 4],
}

impl IconVertex {
    const fn new(clip_pos: [f32; 4], uv: [f32; 2], layer: u32) -> Self {
        Self {
            clip_pos,
            uv,
            layer,
            _pad: [0; 4],
        }
    }
}

/// Pure byte-offset description of [`IconVertex`], consumed by the icon
/// pipeline's `VertexBufferLayout`. Kept GPU-free so tests can pin the ABI
/// without constructing a device.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct IconVertexLayout {
    pub stride: usize,
    pub clip_pos_offset: usize,
    pub uv_offset: usize,
    pub layer_offset: usize,
}

/// The one and only vertex layout for [`IconVertex`].
pub const ICON_VERTEX_LAYOUT: IconVertexLayout = IconVertexLayout {
    stride: std::mem::size_of::<IconVertex>(),
    clip_pos_offset: std::mem::offset_of!(IconVertex, clip_pos),
    uv_offset: std::mem::offset_of!(IconVertex, uv),
    layer_offset: std::mem::offset_of!(IconVertex, layer),
};

/// Returns [`ICON_VERTEX_LAYOUT`]; a function form keeps call sites reading
/// like the pure geometry helpers around it.
pub const fn icon_vertex_layout() -> IconVertexLayout {
    ICON_VERTEX_LAYOUT
}

/// Texture-array layers for the three faces of a block icon cube.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BlockIconLayers {
    pub top: u8,
    pub left: u8,
    pub right: u8,
}

/// Maps a block id onto the three visible cube-face layers.
///
/// The faces reuse the existing block texture-array layers: top is face
/// direction 2 (+Y), left is 4 (+Z), and right is 0 (+X). A later slice that
/// adds a real crafting-table texture only has to teach
/// [`BlockId::face_tex_layer`] the new layer; every icon path already routes
/// through this function, so the mapping never needs duplicating.
#[inline]
pub const fn block_icon_layers(block: BlockId) -> BlockIconLayers {
    BlockIconLayers {
        top: block.face_tex_layer(2),
        left: block.face_tex_layer(4),
        right: block.face_tex_layer(0),
    }
}

// ---------------------------------------------------------------------------
// Procedural item sprites
// ---------------------------------------------------------------------------

/// Number of layers in the UI-only procedural item sprite texture array.
pub const ITEM_SPRITE_LAYERS: u32 = 32;

/// Edge length, in texels, of every procedural item sprite layer.
pub const ITEM_SPRITE_SIZE: u32 = 16;

/// Byte length of one RGBA sprite layer (`16 * 16 * 4`).
pub const ITEM_SPRITE_BYTES: usize = (ITEM_SPRITE_SIZE * ITEM_SPRITE_SIZE * 4) as usize;

/// Where a carryable item's icon geometry and texture come from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum IconSource {
    /// Three-face cube sampled from the world block texture array.
    Block(BlockIconLayers),
    /// Axis-aligned quad sampled from the UI-only procedural sprite array.
    Sprite(u32),
}

/// Append-only, index-pinned item sprite table.
///
/// These indices are a persistent contract: the sprite array is uploaded once
/// per renderer and every icon batch refers to a layer by number. Never
/// reorder, renumber, or reuse a slot; append new identities at the end (or in
/// the next free block of their own kind).
const ITEM_SPRITE_TABLE: [(ItemId, u32); 27] = [
    (ItemId::Material(Material::Stick), 0),
    (ItemId::Material(Material::Apple), 1),
    (ItemId::Material(Material::Wheat), 2),
    (ItemId::Material(Material::WheatSeeds), 3),
    (ItemId::Material(Material::Bread), 4),
    (ItemId::Material(Material::Leather), 5),
    (ItemId::Material(Material::RawPorkchop), 6),
    (ItemId::Material(Material::RawBeef), 7),
    (ItemId::Material(Material::RottenFlesh), 8),
    (ItemId::Material(Material::Bone), 9),
    (ItemId::Material(Material::Arrow), 10),
    (
        ItemId::Tool {
            kind: ToolKind::Pickaxe,
            tier: ToolTier::Wood,
        },
        11,
    ),
    (
        ItemId::Tool {
            kind: ToolKind::Pickaxe,
            tier: ToolTier::Stone,
        },
        12,
    ),
    (
        ItemId::Tool {
            kind: ToolKind::Pickaxe,
            tier: ToolTier::Iron,
        },
        13,
    ),
    (
        ItemId::Tool {
            kind: ToolKind::Axe,
            tier: ToolTier::Wood,
        },
        14,
    ),
    (
        ItemId::Tool {
            kind: ToolKind::Axe,
            tier: ToolTier::Stone,
        },
        15,
    ),
    (
        ItemId::Tool {
            kind: ToolKind::Axe,
            tier: ToolTier::Iron,
        },
        16,
    ),
    (
        ItemId::Tool {
            kind: ToolKind::Shovel,
            tier: ToolTier::Wood,
        },
        17,
    ),
    (
        ItemId::Tool {
            kind: ToolKind::Shovel,
            tier: ToolTier::Stone,
        },
        18,
    ),
    (
        ItemId::Tool {
            kind: ToolKind::Shovel,
            tier: ToolTier::Iron,
        },
        19,
    ),
    (
        ItemId::Tool {
            kind: ToolKind::Hoe,
            tier: ToolTier::Wood,
        },
        20,
    ),
    (
        ItemId::Tool {
            kind: ToolKind::Hoe,
            tier: ToolTier::Stone,
        },
        21,
    ),
    (
        ItemId::Tool {
            kind: ToolKind::Hoe,
            tier: ToolTier::Iron,
        },
        22,
    ),
    (
        ItemId::Tool {
            kind: ToolKind::Sword,
            tier: ToolTier::Wood,
        },
        23,
    ),
    (
        ItemId::Tool {
            kind: ToolKind::Sword,
            tier: ToolTier::Stone,
        },
        24,
    ),
    (
        ItemId::Tool {
            kind: ToolKind::Sword,
            tier: ToolTier::Iron,
        },
        25,
    ),
    (
        ItemId::Tool {
            kind: ToolKind::Bow,
            tier: ToolTier::Wood,
        },
        26,
    ),
];

/// Procedural sprite layer for a material or constructible tool identity.
///
/// Blocks use the block texture array instead and return `None`. An
/// unconstructible tool pair (for example a stone bow) also returns `None`, so
/// a forged identity can never select a sprite that does not exist.
pub fn item_sprite_layer(id: ItemId) -> Option<u32> {
    if let ItemId::Tool { kind, tier } = id {
        if !kind.allowed_tiers().contains(&tier) {
            return None;
        }
    }
    if matches!(id, ItemId::Block(_)) {
        return None;
    }
    ITEM_SPRITE_TABLE
        .iter()
        .find_map(|(entry, layer)| (*entry == id).then_some(*layer))
}

/// Total icon-source mapping for every item identity.
///
/// Carryable blocks yield [`IconSource::Block`]; materials and constructible
/// tools yield [`IconSource::Sprite`]. Uncarryable blocks and unconstructible
/// tool pairs yield `None`. Every carryable identity has exactly one source,
/// so a slot can never silently render empty.
pub fn icon_source(id: ItemId) -> Option<IconSource> {
    match id {
        ItemId::Block(block) => block
            .is_carryable()
            .then(|| IconSource::Block(block_icon_layers(block))),
        ItemId::Material(_) | ItemId::Tool { .. } => item_sprite_layer(id).map(IconSource::Sprite),
    }
}

// Sprite palette. Every color is opaque; the canvas starts fully transparent.
const WOOD: [u8; 4] = [0x8B, 0x5A, 0x2B, 0xFF];
const WOOD_LIGHT: [u8; 4] = [0xB0, 0x7A, 0x3E, 0xFF];
const WOOD_DARK: [u8; 4] = [0x5C, 0x3A, 0x1B, 0xFF];
const APPLE_RED: [u8; 4] = [0xD3, 0x2F, 0x2F, 0xFF];
const APPLE_LIGHT: [u8; 4] = [0xFF, 0x70, 0x70, 0xFF];
const APPLE_DARK: [u8; 4] = [0x8E, 0x1A, 0x1A, 0xFF];
const LEAF: [u8; 4] = [0x3F, 0xA3, 0x3F, 0xFF];
const WHEAT_GOLD: [u8; 4] = [0xE0, 0xB8, 0x4C, 0xFF];
const WHEAT_DARK: [u8; 4] = [0xB8, 0x8A, 0x2E, 0xFF];
const STALK: [u8; 4] = [0x8C, 0x7A, 0x2E, 0xFF];
const SEED: [u8; 4] = [0xD8, 0xB9, 0x6E, 0xFF];
const SEED_DARK: [u8; 4] = [0x8C, 0x6B, 0x2F, 0xFF];
const BREAD: [u8; 4] = [0xC8, 0x8A, 0x45, 0xFF];
const BREAD_LIGHT: [u8; 4] = [0xE6, 0xB8, 0x74, 0xFF];
const BREAD_DARK: [u8; 4] = [0x8B, 0x5A, 0x2B, 0xFF];
const LEATHER: [u8; 4] = [0x9B, 0x5A, 0x2B, 0xFF];
const LEATHER_DARK: [u8; 4] = [0x6B, 0x3A, 0x18, 0xFF];
const LEATHER_LIGHT: [u8; 4] = [0xC8, 0x8A, 0x55, 0xFF];
const PORK: [u8; 4] = [0xE8, 0x8B, 0x9A, 0xFF];
const PORK_DARK: [u8; 4] = [0xC0, 0x5F, 0x70, 0xFF];
const FAT: [u8; 4] = [0xF7, 0xE3, 0xE3, 0xFF];
const BEEF: [u8; 4] = [0xA8, 0x33, 0x33, 0xFF];
const BEEF_DARK: [u8; 4] = [0x7A, 0x1F, 0x1F, 0xFF];
const BEEF_FAT: [u8; 4] = [0xE8, 0xC9, 0xB0, 0xFF];
const ROTTEN: [u8; 4] = [0x7D, 0x8B, 0x3A, 0xFF];
const ROTTEN_DARK: [u8; 4] = [0x4E, 0x5C, 0x24, 0xFF];
const ROTTEN_BROWN: [u8; 4] = [0x8A, 0x63, 0x33, 0xFF];
const BONE: [u8; 4] = [0xE8, 0xE0, 0xC8, 0xFF];
const BONE_SHADOW: [u8; 4] = [0xB8, 0xAE, 0x94, 0xFF];
const ARROW_SHAFT: [u8; 4] = [0x8B, 0x5A, 0x2B, 0xFF];
const ARROW_HEAD: [u8; 4] = [0xB8, 0xBC, 0xC0, 0xFF];
const FLETCH_WHITE: [u8; 4] = [0xF0, 0xF0, 0xF0, 0xFF];
const FLETCH_RED: [u8; 4] = [0xC0, 0x30, 0x30, 0xFF];
const TIER_WOOD: [u8; 4] = [0xA9, 0x74, 0x38, 0xFF];
const TIER_STONE: [u8; 4] = [0x8C, 0x8C, 0x8C, 0xFF];
const TIER_IRON: [u8; 4] = [0xD9, 0xDD, 0xE2, 0xFF];
const STRING: [u8; 4] = [0xD8, 0xD8, 0xD8, 0xFF];

/// Tiny deterministic RGBA pixel canvas. The background starts fully
/// transparent so the inventory/hotbar slot shows through every sprite.
struct SpriteCanvas {
    pixels: Vec<u8>,
}

impl SpriteCanvas {
    fn new() -> Self {
        Self {
            pixels: vec![0; ITEM_SPRITE_BYTES],
        }
    }

    fn pixel(&mut self, x: i32, y: i32, color: [u8; 4]) {
        if x < 0 || y < 0 || x >= ITEM_SPRITE_SIZE as i32 || y >= ITEM_SPRITE_SIZE as i32 {
            return;
        }
        let index = ((y as usize) * ITEM_SPRITE_SIZE as usize + x as usize) * 4;
        self.pixels[index..index + 4].copy_from_slice(&color);
    }

    fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: [u8; 4]) {
        if w <= 0 || h <= 0 {
            return;
        }
        for py in y..y + h {
            for px in x..x + w {
                self.pixel(px, py, color);
            }
        }
    }

    /// Bresenham line, one pixel wide.
    fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: [u8; 4]) {
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        let (mut x, mut y) = (x0, y0);
        loop {
            self.pixel(x, y, color);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// Stamp a square brush along a line.
    fn line_thick(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, thickness: i32, color: [u8; 4]) {
        let thickness = thickness.max(1);
        for offset in 0..thickness {
            self.line(x0 + offset, y0 + offset, x1 + offset, y1 + offset, color);
        }
    }

    fn into_rgba(self) -> Vec<u8> {
        self.pixels
    }
}

/// Fill one-pixel-tall centered rows, top-down, from an array of widths.
fn fill_centered_rows(canvas: &mut SpriteCanvas, top: i32, widths: &[i32], color: [u8; 4]) {
    for (offset, width) in widths.iter().enumerate() {
        let x = ITEM_SPRITE_SIZE as i32 / 2 - width / 2;
        canvas.fill_rect(x, top + offset as i32, *width, 1, color);
    }
}

const fn tier_color(tier: ToolTier) -> [u8; 4] {
    match tier {
        ToolTier::Wood => TIER_WOOD,
        ToolTier::Stone => TIER_STONE,
        ToolTier::Iron => TIER_IRON,
    }
}

const fn tier_light(tier: ToolTier) -> [u8; 4] {
    match tier {
        ToolTier::Wood => [0xC9, 0x94, 0x52, 0xFF],
        ToolTier::Stone => [0xB0, 0xB0, 0xB0, 0xFF],
        ToolTier::Iron => [0xF2, 0xF5, 0xF8, 0xFF],
    }
}

const fn tier_dark(tier: ToolTier) -> [u8; 4] {
    match tier {
        ToolTier::Wood => [0x7A, 0x4E, 0x22, 0xFF],
        ToolTier::Stone => [0x5F, 0x5F, 0x5F, 0xFF],
        ToolTier::Iron => [0x9A, 0xA0, 0xA8, 0xFF],
    }
}

fn draw_stick(canvas: &mut SpriteCanvas) {
    for step in 0..9 {
        let x = 3 + step;
        let y = 12 - step;
        canvas.fill_rect(x, y, 2, 2, WOOD);
        canvas.pixel(x, y, WOOD_LIGHT);
    }
    canvas.pixel(6, 8, WOOD_DARK);
    canvas.pixel(9, 5, WOOD_DARK);
}

fn draw_apple(canvas: &mut SpriteCanvas) {
    fill_centered_rows(canvas, 5, &[6, 8, 10, 10, 10, 8, 6, 4], APPLE_RED);
    canvas.fill_rect(5, 7, 2, 2, APPLE_LIGHT);
    canvas.pixel(11, 10, APPLE_DARK);
    canvas.pixel(10, 11, APPLE_DARK);
    canvas.pixel(9, 12, APPLE_DARK);
    canvas.fill_rect(8, 2, 2, 3, WOOD_DARK);
    canvas.fill_rect(10, 2, 3, 2, LEAF);
    canvas.pixel(10, 2, [0x6F, 0xC8, 0x6F, 0xFF]);
}

fn draw_wheat(canvas: &mut SpriteCanvas) {
    for x in [4, 8, 12] {
        canvas.line(x, 13, x, 4, STALK);
        canvas.pixel(x, 3, WHEAT_GOLD);
        for y in [4, 6, 8, 10] {
            canvas.fill_rect(x - 1, y, 2, 2, WHEAT_GOLD);
            canvas.fill_rect(x + 1, y + 1, 2, 2, WHEAT_DARK);
        }
    }
}

fn draw_wheat_seeds(canvas: &mut SpriteCanvas) {
    for (x, y) in [(3, 3), (8, 2), (5, 8), (10, 7), (7, 12), (11, 11)] {
        canvas.fill_rect(x, y, 2, 3, SEED);
        canvas.pixel(x + 1, y + 2, SEED_DARK);
    }
}

fn draw_bread(canvas: &mut SpriteCanvas) {
    fill_centered_rows(canvas, 5, &[6, 8, 10, 10, 10, 10, 8, 6], BREAD);
    canvas.fill_rect(5, 5, 6, 1, BREAD_LIGHT);
    canvas.line(6, 8, 7, 9, BREAD_DARK);
    canvas.line(9, 8, 10, 9, BREAD_DARK);
    canvas.line(12, 8, 13, 9, BREAD_DARK);
    canvas.fill_rect(5, 12, 6, 1, BREAD_DARK);
}

fn draw_leather(canvas: &mut SpriteCanvas) {
    fill_centered_rows(canvas, 4, &[6, 10, 10, 10, 10, 10, 10, 8], LEATHER);
    canvas.fill_rect(5, 4, 6, 1, LEATHER_DARK);
    canvas.fill_rect(4, 11, 8, 1, LEATHER_DARK);
    for x in [5, 7, 9, 11] {
        canvas.pixel(x, 6, LEATHER_LIGHT);
        canvas.pixel(x, 10, LEATHER_LIGHT);
    }
    canvas.fill_rect(7, 7, 2, 2, LEATHER_DARK);
}

fn draw_raw_porkchop(canvas: &mut SpriteCanvas) {
    fill_centered_rows(canvas, 5, &[7, 10, 10, 10, 10, 10, 8], PORK);
    canvas.fill_rect(3, 7, 8, 1, FAT);
    canvas.fill_rect(4, 9, 7, 1, FAT);
    canvas.fill_rect(5, 11, 4, 1, PORK_DARK);
    canvas.fill_rect(2, 8, 2, 3, BONE);
    canvas.pixel(2, 8, BONE_SHADOW);
}

fn draw_raw_beef(canvas: &mut SpriteCanvas) {
    fill_centered_rows(canvas, 5, &[6, 10, 10, 10, 10, 10, 8], BEEF);
    canvas.line(5, 7, 8, 9, BEEF_FAT);
    canvas.line(9, 6, 12, 8, BEEF_FAT);
    canvas.fill_rect(5, 10, 5, 1, BEEF_DARK);
    canvas.pixel(11, 9, BEEF_DARK);
}

fn draw_rotten_flesh(canvas: &mut SpriteCanvas) {
    fill_centered_rows(canvas, 4, &[5, 8, 10, 10, 10, 8, 6], ROTTEN);
    canvas.fill_rect(5, 6, 2, 2, ROTTEN_DARK);
    canvas.fill_rect(10, 8, 2, 2, ROTTEN_BROWN);
    canvas.fill_rect(7, 10, 2, 1, ROTTEN_DARK);
    canvas.pixel(4, 8, ROTTEN_BROWN);
    canvas.pixel(11, 5, ROTTEN_DARK);
}

fn draw_bone(canvas: &mut SpriteCanvas) {
    canvas.fill_rect(7, 4, 2, 8, BONE);
    canvas.fill_rect(5, 2, 3, 3, BONE);
    canvas.fill_rect(9, 2, 3, 3, BONE);
    canvas.fill_rect(5, 11, 3, 3, BONE);
    canvas.fill_rect(9, 11, 3, 3, BONE);
    canvas.fill_rect(10, 4, 1, 7, BONE_SHADOW);
    canvas.pixel(11, 4, BONE_SHADOW);
    canvas.pixel(7, 11, BONE_SHADOW);
    canvas.pixel(5, 13, BONE_SHADOW);
}

fn draw_arrow(canvas: &mut SpriteCanvas) {
    canvas.line_thick(3, 13, 10, 6, 1, ARROW_SHAFT);
    canvas.fill_rect(9, 4, 2, 2, ARROW_HEAD);
    canvas.fill_rect(10, 3, 2, 2, ARROW_HEAD);
    canvas.fill_rect(11, 2, 2, 2, ARROW_HEAD);
    canvas.pixel(13, 2, ARROW_HEAD);
    canvas.fill_rect(2, 12, 2, 3, FLETCH_WHITE);
    canvas.fill_rect(3, 14, 2, 2, FLETCH_RED);
    canvas.pixel(2, 15, FLETCH_WHITE);
}

fn draw_handle(canvas: &mut SpriteCanvas) {
    canvas.line_thick(3, 13, 10, 6, 2, WOOD);
    canvas.line(3, 13, 10, 6, WOOD_LIGHT);
    canvas.pixel(4, 14, WOOD_DARK);
}

fn draw_pickaxe(canvas: &mut SpriteCanvas, tier: ToolTier) {
    draw_handle(canvas);
    let head = tier_color(tier);
    canvas.fill_rect(5, 3, 8, 2, head);
    canvas.fill_rect(4, 4, 2, 2, head);
    canvas.fill_rect(12, 4, 2, 2, head);
    canvas.fill_rect(5, 3, 7, 1, tier_light(tier));
    canvas.fill_rect(6, 5, 5, 1, tier_dark(tier));
}

fn draw_axe(canvas: &mut SpriteCanvas, tier: ToolTier) {
    draw_handle(canvas);
    let head = tier_color(tier);
    canvas.fill_rect(9, 2, 4, 7, head);
    canvas.fill_rect(12, 3, 1, 5, tier_light(tier));
    canvas.fill_rect(9, 8, 3, 1, tier_dark(tier));
    canvas.pixel(8, 3, head);
    canvas.pixel(8, 4, head);
}

fn draw_shovel(canvas: &mut SpriteCanvas, tier: ToolTier) {
    draw_handle(canvas);
    let head = tier_color(tier);
    canvas.fill_rect(9, 2, 3, 2, head);
    canvas.fill_rect(8, 4, 5, 3, head);
    canvas.fill_rect(9, 7, 3, 1, head);
    canvas.fill_rect(9, 3, 2, 2, tier_light(tier));
    canvas.fill_rect(9, 7, 3, 1, tier_dark(tier));
}

fn draw_hoe(canvas: &mut SpriteCanvas, tier: ToolTier) {
    draw_handle(canvas);
    let head = tier_color(tier);
    canvas.fill_rect(7, 3, 7, 2, head);
    canvas.fill_rect(12, 5, 2, 3, head);
    canvas.fill_rect(7, 3, 6, 1, tier_light(tier));
    canvas.fill_rect(13, 5, 1, 3, tier_dark(tier));
}

fn draw_sword(canvas: &mut SpriteCanvas, tier: ToolTier) {
    let blade = tier_color(tier);
    canvas.fill_rect(7, 1, 2, 9, blade);
    canvas.fill_rect(7, 0, 2, 1, blade);
    canvas.fill_rect(7, 1, 1, 8, tier_light(tier));
    canvas.fill_rect(8, 2, 1, 7, tier_dark(tier));
    canvas.fill_rect(4, 10, 8, 2, tier_dark(tier));
    canvas.fill_rect(4, 10, 8, 1, tier_light(tier));
    canvas.fill_rect(7, 12, 2, 3, WOOD);
    canvas.fill_rect(7, 12, 1, 3, WOOD_LIGHT);
    canvas.fill_rect(6, 15, 4, 1, WOOD_DARK);
}

fn draw_bow(canvas: &mut SpriteCanvas) {
    let points = [
        (5, 13),
        (4, 11),
        (4, 9),
        (4, 7),
        (5, 5),
        (6, 4),
        (7, 3),
        (8, 2),
    ];
    for pair in points.windows(2) {
        canvas.line_thick(pair[0].0, pair[0].1, pair[1].0, pair[1].1, 2, WOOD);
    }
    for (x, y) in points {
        canvas.pixel(x, y, WOOD_LIGHT);
    }
    canvas.fill_rect(3, 8, 2, 3, WOOD_DARK);
    canvas.line(5, 13, 8, 2, STRING);
    canvas.pixel(8, 2, STRING);
    canvas.pixel(5, 13, STRING);
}

fn draw_material(material: Material, canvas: &mut SpriteCanvas) {
    match material {
        Material::Stick => draw_stick(canvas),
        Material::Apple => draw_apple(canvas),
        Material::Wheat => draw_wheat(canvas),
        Material::WheatSeeds => draw_wheat_seeds(canvas),
        Material::Bread => draw_bread(canvas),
        Material::Leather => draw_leather(canvas),
        Material::RawPorkchop => draw_raw_porkchop(canvas),
        Material::RawBeef => draw_raw_beef(canvas),
        Material::RottenFlesh => draw_rotten_flesh(canvas),
        Material::Bone => draw_bone(canvas),
        Material::Arrow => draw_arrow(canvas),
    }
}

fn draw_item(id: ItemId, canvas: &mut SpriteCanvas) {
    match id {
        ItemId::Block(_) => {}
        ItemId::Material(material) => draw_material(material, canvas),
        ItemId::Tool { kind, tier } => {
            if !kind.allowed_tiers().contains(&tier) {
                return;
            }
            match kind {
                ToolKind::Pickaxe => draw_pickaxe(canvas, tier),
                ToolKind::Axe => draw_axe(canvas, tier),
                ToolKind::Shovel => draw_shovel(canvas, tier),
                ToolKind::Hoe => draw_hoe(canvas, tier),
                ToolKind::Sword => draw_sword(canvas, tier),
                ToolKind::Bow => draw_bow(canvas),
            }
        }
    }
}

/// Generate every procedural item sprite layer as tightly packed RGBA bytes.
///
/// Returns exactly [`ITEM_SPRITE_LAYERS`] layers of [`ITEM_SPRITE_BYTES`]
/// bytes. The result is deterministic: no randomness, clocks, or floating
/// point, so two calls always compare byte-for-byte. Layers `27..32` are
/// reserved and stay fully transparent.
pub fn generate_item_sprite_layers_rgba() -> Vec<Vec<u8>> {
    let mut layers = vec![vec![0u8; ITEM_SPRITE_BYTES]; ITEM_SPRITE_LAYERS as usize];
    for (id, layer) in ITEM_SPRITE_TABLE {
        let mut canvas = SpriteCanvas::new();
        draw_item(id, &mut canvas);
        layers[layer as usize] = canvas.into_rgba();
    }
    layers
}

/// Inset one slot rect into the icon area.
///
/// The inset is a deterministic fraction of the smaller edge, so square and
/// non-square slots both get breathing room. Degenerate input (zero, negative,
/// or non-finite dimensions) collapses to a finite empty rect instead of
/// producing an inverted one.
pub fn icon_rect((x, y, w, h): Rect) -> Rect {
    let x = if x.is_finite() { x } else { 0.0 };
    let y = if y.is_finite() { y } else { 0.0 };
    let w = if w.is_finite() { w.max(0.0) } else { 0.0 };
    let h = if h.is_finite() { h.max(0.0) } else { 0.0 };
    if w == 0.0 || h == 0.0 {
        return (x, y, 0.0, 0.0);
    }

    let inset = ICON_INSET_FRACTION * w.min(h);
    (
        x + inset,
        y + inset,
        (w - 2.0 * inset).max(0.0),
        (h - 2.0 * inset).max(0.0),
    )
}

/// Emit one pixel-space quad as two triangles (corner order 0,1,2 then 2,3,0),
/// carrying full 0..=1 UVs and the given texture-array layer.
fn push_quad(
    out: &mut Vec<IconVertex>,
    corners: [(f32, f32); 4],
    layer: u32,
    surface_w: f32,
    surface_h: f32,
) {
    let uvs = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
    let vertex = |index: usize| {
        let (px, py) = corners[index];
        IconVertex::new(
            [
                2.0 * px / surface_w - 1.0,
                1.0 - 2.0 * py / surface_h,
                0.0,
                1.0,
            ],
            [uvs[index].0, uvs[index].1],
            layer,
        )
    };
    let c0 = vertex(0);
    let c1 = vertex(1);
    let c2 = vertex(2);
    let c3 = vertex(3);
    out.extend([c0, c1, c2, c2, c3, c0]);
}

/// Build the 18 vertices of a three-face block cube icon inside `slot`.
///
/// Face order is top, left, right (6 vertices each). The cube silhouette fills
/// [`icon_rect`] exactly: a top rhombus over two side parallelograms, sharing
/// edges but never overlapping. Every coordinate is finite even for 0x0 and
/// 1x1 surfaces because the surface dimensions are clamped to at least one.
pub fn cube_icon_vertices(
    slot: Rect,
    layers: BlockIconLayers,
    surface_w: u32,
    surface_h: u32,
) -> Vec<IconVertex> {
    let (ix, iy, iw, ih) = icon_rect(slot);
    let sw = surface_w.max(1) as f32;
    let sh = surface_h.max(1) as f32;

    let cx = ix + iw * 0.5;
    let half_w = iw * 0.5;
    let top_h = ih * 0.25;
    let side_h = ih * 0.5;

    let top = (cx, iy);
    let right = (cx + half_w, iy + top_h);
    let bottom = (cx, iy + 2.0 * top_h);
    let left = (cx - half_w, iy + top_h);
    let bottom_left = (cx - half_w, iy + top_h + side_h);
    let bottom_right = (cx + half_w, iy + top_h + side_h);
    let bottom_mid = (cx, iy + 2.0 * top_h + side_h);

    let mut verts = Vec::with_capacity(18);
    // Top rhombus.
    push_quad(
        &mut verts,
        [top, right, bottom, left],
        layers.top as u32,
        sw,
        sh,
    );
    // Left side (faces +Z / south).
    push_quad(
        &mut verts,
        [left, bottom, bottom_mid, bottom_left],
        layers.left as u32,
        sw,
        sh,
    );
    // Right side (faces +X / east).
    push_quad(
        &mut verts,
        [bottom, right, bottom_right, bottom_mid],
        layers.right as u32,
        sw,
        sh,
    );
    verts
}

/// Icon vertices for one inventory item, or `None` when the item has no cube
/// icon in this slice.
///
/// Only carryable block identities produce a cube; materials and tools return
/// `None` until their own icon path lands. Empty or uncarryable items also
/// return `None`, so a malformed caller can never draw a ghost cube.
pub fn icon_vertices_for_item(
    item: Item,
    slot: Rect,
    surface_w: u32,
    surface_h: u32,
) -> Option<Vec<IconVertex>> {
    match item.id() {
        ItemId::Block(block) if item.count() > 0 && block.is_carryable() => Some(
            cube_icon_vertices(slot, block_icon_layers(block), surface_w, surface_h),
        ),
        _ => None,
    }
}

/// Append the cube icon for one optional item into `out`, skipping empty and
/// non-block identities (S01 only draws block cubes).
fn push_item_icon(
    out: &mut Vec<IconVertex>,
    item: Option<Item>,
    slot: Rect,
    surface_w: u32,
    surface_h: u32,
) {
    if let Some(item) = item {
        if let Some(vertices) = icon_vertices_for_item(item, slot, surface_w, surface_h) {
            out.extend(vertices);
        }
    }
}

/// Append the cube icon for one recipe preview's output, if it is a carryable
/// block and the output stack is valid.
fn push_preview_icon(
    out: &mut Vec<IconVertex>,
    preview: Option<crate::player::inventory::RecipePreview>,
    slot: Rect,
    surface_w: u32,
    surface_h: u32,
) {
    let Some(preview) = preview else {
        return;
    };
    let item = Item::new(preview.output, preview.output_count).ok();
    push_item_icon(out, item, slot, surface_w, surface_h);
}

/// All block cube icons for the open inventory panel.
///
/// Covers exactly the same surfaces as [`super::inventory::screen_quads`]:
/// 36 inventory slots, the 4 player crafting cells, the 16 table cells only
/// in [`InventoryMode::Table`], both result previews, and the cursor ghost.
/// Closed screens emit nothing. Non-block identities are skipped by
/// [`icon_vertices_for_item`] until their own icon path lands.
#[allow(clippy::too_many_arguments)]
pub fn inventory_icon_vertices(
    mode: InventoryMode,
    layout: &InventoryLayout,
    inventory: &Inventory,
    player_grid: &CraftingGrid,
    table_grid: &CraftingGrid,
    cursor_item: Option<Item>,
    cursor_pos: Option<(f32, f32)>,
    surface_w: u32,
    surface_h: u32,
) -> Vec<IconVertex> {
    if !mode.is_open() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for (index, rect) in layout.slots.iter().enumerate() {
        push_item_icon(&mut out, inventory.slot(index), *rect, surface_w, surface_h);
    }
    for (index, rect) in layout.player_grid.iter().enumerate() {
        push_item_icon(
            &mut out,
            player_grid.slot(index),
            *rect,
            surface_w,
            surface_h,
        );
    }
    push_preview_icon(
        &mut out,
        player_grid.preview(),
        layout.player_result,
        surface_w,
        surface_h,
    );

    if mode == InventoryMode::Table {
        for (index, rect) in layout.table_grid.iter().enumerate() {
            push_item_icon(
                &mut out,
                table_grid.slot(index),
                *rect,
                surface_w,
                surface_h,
            );
        }
        push_preview_icon(
            &mut out,
            table_grid.preview(),
            layout.table_result,
            surface_w,
            surface_h,
        );
    }

    if let (Some(item), Some((cx, cy))) = (cursor_item, cursor_pos) {
        if cx.is_finite() && cy.is_finite() {
            let size = layout.player_result.2 * 0.75;
            push_item_icon(
                &mut out,
                Some(item),
                (cx - size * 0.5, cy - size * 0.5, size, size),
                surface_w,
                surface_h,
            );
        }
    }

    out
}

/// Block cube icons for the nine gameplay hotbar slots.
///
/// Each icon is inset inside its [`hotbar_slot_rects`] slot and clipped to
/// stay above the count label's top edge ([`hotbar_count_text_origin`]), so a
/// stack count can never be hidden behind a cube face. Empty slots and
/// non-block identities produce no vertices.
pub fn hotbar_icon_vertices(
    inventory: &Inventory,
    surface_w: u32,
    surface_h: u32,
) -> Vec<IconVertex> {
    let mut out = Vec::new();
    for (slot, rect) in hotbar_slot_rects(surface_w, surface_h).iter().enumerate() {
        let icon_slot = hotbar_icon_rect(*rect);
        push_item_icon(
            &mut out,
            inventory.hotbar(slot),
            icon_slot,
            surface_w,
            surface_h,
        );
    }
    out
}

/// Filter a CPU icon batch down to the vertices the GPU can safely consume.
///
/// Inputs are already in clip space, so the surface dimensions do not affect
/// the result; they stay in the signature to mirror the other UI upload
/// helpers and make the caller's "this batch belongs to this surface"
/// contract explicit. Non-finite vertices are dropped rather than uploaded,
/// which keeps a malformed caller from poisoning the whole buffer.
pub fn sanitize_icon_vertices(
    icons: &[IconVertex],
    _surface_w: u32,
    _surface_h: u32,
) -> Vec<IconVertex> {
    icons
        .iter()
        .copied()
        .filter(|v| v.clip_pos.iter().all(|c| c.is_finite()))
        .collect()
}

/// Textured UI icon shader: `IconVertex` in, one `texture_2d_array` sample
/// out, with the texture-array layer carried per vertex.
const ICON_SHADER_WGSL: &str = r#"
struct IconOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) layer: u32,
};

@group(0) @binding(0) var tex: texture_2d_array<f32>;
@group(0) @binding(1) var samp: sampler;

@vertex
fn vs_main(
    @location(0) clip_pos: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) layer: u32,
) -> IconOutput {
    var out: IconOutput;
    out.clip_pos = clip_pos;
    out.uv = uv;
    out.layer = layer;
    return out;
}

@fragment
fn fs_main(in: IconOutput) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv, i32(in.layer));
}
"#;

/// Draws textured item icons (block cube faces) inside the color-only UI
/// pass, sampling the world's block texture array.
///
/// The texture view/sampler are borrowed once at construction (they come from
/// [`crate::render::pipeline::VoxelRenderer`]) and then owned by the bind
/// group. Only the per-frame vertex buffer is rebuilt in
/// [`set_icons`](Self::set_icons), the same pattern as `QuadRenderer` and
/// `LogoRenderer`.
///
/// Draw this strictly after the quad and logo renderers and strictly before
/// glyphon text, so icons sit above panels/tint and below labels/counts.
pub struct IconRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: Option<wgpu::BindGroup>,
    vertex_buffer: Option<wgpu::Buffer>,
    vertex_count: u32,
}

impl IconRenderer {
    pub fn new(
        ctx: &RenderContext,
        texture_view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) -> Self {
        let shader = ctx
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("ui_item_icon_shader"),
                source: wgpu::ShaderSource::Wgsl(ICON_SHADER_WGSL.into()),
            });

        let bind_group_layout =
            ctx.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("ui_item_icon_bind_group_layout"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2Array,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });

        let layout = ctx
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("ui_item_icon_pipeline_layout"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });

        let pipeline = ctx
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("ui_item_icon_pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs_main",
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: ICON_VERTEX_LAYOUT.stride as wgpu::BufferAddress,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                offset: ICON_VERTEX_LAYOUT.clip_pos_offset as wgpu::BufferAddress,
                                shader_location: 0,
                                format: wgpu::VertexFormat::Float32x4,
                            },
                            wgpu::VertexAttribute {
                                offset: ICON_VERTEX_LAYOUT.uv_offset as wgpu::BufferAddress,
                                shader_location: 1,
                                format: wgpu::VertexFormat::Float32x2,
                            },
                            wgpu::VertexAttribute {
                                offset: ICON_VERTEX_LAYOUT.layer_offset as wgpu::BufferAddress,
                                shader_location: 2,
                                format: wgpu::VertexFormat::Uint32,
                            },
                        ],
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "fs_main",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: ctx.config.format,
                        // Straight-alpha blending, same convention as quads
                        // and the logo: pack art composites over panels.
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });

        let bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui_item_icon_bind_group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });

        Self {
            pipeline,
            bind_group: Some(bind_group),
            vertex_buffer: None,
            vertex_count: 0,
        }
    }

    /// Rebuild the per-frame icon vertex buffer. Pure CPU work (no texture
    /// upload); call once per frame before the UI pass opens. An empty slice
    /// clears the buffer so nothing is drawn.
    pub fn set_icons(
        &mut self,
        device: &wgpu::Device,
        icons: &[IconVertex],
        surface_w: u32,
        surface_h: u32,
    ) {
        let verts = sanitize_icon_vertices(icons, surface_w, surface_h);
        self.vertex_count = verts.len() as u32;
        self.vertex_buffer = if verts.is_empty() {
            None
        } else {
            Some(
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("ui_item_icon_vbuf"),
                    contents: bytemuck::cast_slice(&verts),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
            )
        };
    }

    /// Draw pending icons into the UI pass. No-op when nothing is queued.
    pub fn draw<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        if self.vertex_count == 0 {
            return;
        }
        let (Some(bind_group), Some(buffer)) = (&self.bind_group, &self.vertex_buffer) else {
            return;
        };
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.set_vertex_buffer(0, buffer.slice(..));
        pass.draw(0..self.vertex_count, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::inventory::{
        CraftingGrid, GridKind, Inventory, Item, ItemId, Material, ToolKind, ToolTier,
        HOTBAR_SLOTS, INVENTORY_SLOTS,
    };

    const SLOT: Rect = (10.0, 20.0, 40.0, 40.0);

    fn item(id: ItemId) -> Item {
        Item::new(id, 1).expect("test item is valid")
    }

    fn assert_close(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-5,
            "{what}: {actual} != {expected}"
        );
    }

    /// Convert a clip-space vertex back to pixel space for rect containment
    /// checks (inverse of the module's NDC math).
    fn pixel(v: &IconVertex, surface_w: u32, surface_h: u32) -> (f32, f32) {
        let sw = surface_w.max(1) as f32;
        let sh = surface_h.max(1) as f32;
        (
            (v.clip_pos[0] + 1.0) * 0.5 * sw,
            (1.0 - v.clip_pos[1]) * 0.5 * sh,
        )
    }

    #[test]
    fn cube_has_18_vertices_in_top_left_right_face_order() {
        let layers = block_icon_layers(BlockId::Grass);
        let verts = cube_icon_vertices(SLOT, layers, 800, 600);
        assert_eq!(verts.len(), 18);

        // Six vertices per face, in top / left / right order.
        for v in &verts[..6] {
            assert_eq!(v.layer, layers.top as u32, "top face layer");
        }
        for v in &verts[6..12] {
            assert_eq!(v.layer, layers.left as u32, "left face layer");
        }
        for v in &verts[12..] {
            assert_eq!(v.layer, layers.right as u32, "right face layer");
        }
    }

    #[test]
    fn cube_vertices_are_finite_inside_icon_rect_and_uvs_are_unit_range() {
        let (sx, sy, sw, sh) = SLOT;
        let (ix, iy, iw, ih) = icon_rect(SLOT);
        assert!(ix >= sx && iy >= sy);
        assert!(iw <= sw && ih <= sh);

        let verts = cube_icon_vertices(SLOT, block_icon_layers(BlockId::OakLog), 800, 600);
        for (index, v) in verts.iter().enumerate() {
            assert!(v.clip_pos.iter().all(|c| c.is_finite()), "vertex {index}");
            assert_eq!(v.clip_pos[2], 0.0);
            assert_eq!(v.clip_pos[3], 1.0);
            for (axis, uv) in v.uv.iter().enumerate() {
                assert!((0.0..=1.0).contains(uv), "vertex {index} uv[{axis}] = {uv}");
            }

            let (px, py) = pixel(v, 800, 600);
            assert!(
                px >= ix - 1e-4 && px <= ix + iw + 1e-4,
                "vertex {index} x={px} outside [{ix}, {}]",
                ix + iw
            );
            assert!(
                py >= iy - 1e-4 && py <= iy + ih + 1e-4,
                "vertex {index} y={py} outside [{iy}, {}]",
                iy + ih
            );
        }
    }

    #[test]
    fn cube_faces_are_distinct_regions_in_top_left_right_order() {
        // Grass has a distinct top texture and two side textures, so face
        // labels plus centroid ordering prove the quads are laid out as a
        // top rhombus over left/right parallelograms rather than stacked
        // copies of one rect.
        let layers = block_icon_layers(BlockId::Grass);
        let verts = cube_icon_vertices(SLOT, layers, 800, 600);
        assert!(verts[..6].iter().all(|v| v.layer == layers.top as u32));
        assert!(verts[6..].iter().all(|v| v.layer == layers.left as u32));

        let centroid = |slice: &[IconVertex]| {
            let (mut x, mut y) = (0.0, 0.0);
            for v in slice {
                let (px, py) = pixel(v, 800, 600);
                x += px;
                y += py;
            }
            (x / slice.len() as f32, y / slice.len() as f32)
        };
        let (top_x, top_y) = centroid(&verts[..6]);
        let (left_x, left_y) = centroid(&verts[6..12]);
        let (right_x, right_y) = centroid(&verts[12..]);

        assert!(top_x > left_x && top_x < right_x);
        assert!(top_y < left_y && top_y < right_y);
        assert!(left_x < right_x);
    }

    #[test]
    fn icon_vertex_layout_is_pinned_for_the_gpu_stride() {
        let layout = icon_vertex_layout();
        assert_eq!(layout.stride, 32);
        assert_eq!(layout.clip_pos_offset, 0);
        assert_eq!(layout.uv_offset, 16);
        assert_eq!(layout.layer_offset, 24);
        assert_eq!(std::mem::size_of::<IconVertex>(), 32);
    }

    #[test]
    fn block_icon_layers_match_face_texture_mapping() {
        let grass = block_icon_layers(BlockId::Grass);
        assert_eq!((grass.top, grass.left, grass.right), (0, 1, 1));

        let log = block_icon_layers(BlockId::OakLog);
        assert_eq!((log.top, log.left, log.right), (6, 7, 7));

        let table = block_icon_layers(BlockId::CraftingTable);
        assert_eq!((table.top, table.left, table.right), (9, 9, 9));
    }

    #[test]
    fn same_input_yields_identical_vertices() {
        let layers = block_icon_layers(BlockId::Stone);
        let a = cube_icon_vertices(SLOT, layers, 640, 480);
        let b = cube_icon_vertices(SLOT, layers, 640, 480);
        assert_eq!(a.len(), b.len());
        for (va, vb) in a.iter().zip(&b) {
            assert_eq!(va.clip_pos, vb.clip_pos);
            assert_eq!(va.uv, vb.uv);
            assert_eq!(va.layer, vb.layer);
        }
    }

    #[test]
    fn degenerate_surfaces_stay_finite() {
        let layers = block_icon_layers(BlockId::Grass);
        for (sw, sh) in [(0, 0), (1, 1), (0, 480), (640, 0)] {
            let verts = cube_icon_vertices(SLOT, layers, sw, sh);
            assert_eq!(verts.len(), 18);
            for v in &verts {
                assert!(
                    v.clip_pos.iter().all(|c| c.is_finite()),
                    "surface {sw}x{sh} produced a non-finite vertex"
                );
                assert_eq!(v.clip_pos[3], 1.0);
            }
        }
    }

    #[test]
    fn icon_rect_insets_and_never_inverts() {
        let rect = icon_rect((10.0, 20.0, 40.0, 40.0));
        assert_close(rect.0, 16.0, "inset x");
        assert_close(rect.1, 26.0, "inset y");
        assert_close(rect.2, 28.0, "inset width");
        assert_close(rect.3, 28.0, "inset height");

        for slot in [
            (0.0, 0.0, 0.0, 0.0),
            (5.0, 5.0, 1.0, 1.0),
            (5.0, 5.0, -10.0, 4.0),
            (f32::NAN, f32::INFINITY, 10.0, 10.0),
        ] {
            let (x, y, w, h) = icon_rect(slot);
            assert!(x.is_finite() && y.is_finite());
            assert!(w >= 0.0 && h >= 0.0, "never inverted: {w} x {h}");
        }
    }

    #[test]
    fn icon_vertices_for_item_accepts_blocks_and_rejects_other_identities() {
        let grass = item(ItemId::Block(BlockId::Grass));
        let cube = icon_vertices_for_item(grass, SLOT, 800, 600);
        assert_eq!(cube.expect("block has a cube icon").len(), 18);

        let stick = item(ItemId::Material(Material::Stick));
        assert!(icon_vertices_for_item(stick, SLOT, 800, 600).is_none());

        let pickaxe = item(ItemId::Tool {
            kind: ToolKind::Pickaxe,
            tier: ToolTier::Wood,
        });
        assert!(icon_vertices_for_item(pickaxe, SLOT, 800, 600).is_none());
    }

    fn px_vertex(v: &IconVertex, surface_w: u32, surface_h: u32) -> (f32, f32) {
        (
            (v.clip_pos[0] + 1.0) * 0.5 * surface_w.max(1) as f32,
            (1.0 - v.clip_pos[1]) * 0.5 * surface_h.max(1) as f32,
        )
    }

    fn block_inventory(count: usize) -> Inventory {
        let grass = item(ItemId::Block(BlockId::Grass));
        let mut slots = [None; INVENTORY_SLOTS];
        for slot in slots.iter_mut().take(count) {
            *slot = Some(grass);
        }
        Inventory::from_parts(slots, None)
    }

    fn open_layout(mode: InventoryMode) -> crate::ui::inventory::InventoryLayout {
        crate::ui::inventory::layout(1280, 720, mode)
    }

    #[test]
    fn inventory_icons_cover_all_36_slots_and_skip_empty_slots() {
        let full = block_inventory(INVENTORY_SLOTS);
        let player_grid = CraftingGrid::new(GridKind::Player);
        let table_grid = CraftingGrid::new(GridKind::Table);
        let layout = open_layout(InventoryMode::Player);
        let verts = inventory_icon_vertices(
            InventoryMode::Player,
            &layout,
            &full,
            &player_grid,
            &table_grid,
            None,
            None,
            1280,
            720,
        );
        assert_eq!(verts.len(), INVENTORY_SLOTS * 18);

        let empty = Inventory::empty();
        let none = inventory_icon_vertices(
            InventoryMode::Player,
            &layout,
            &empty,
            &player_grid,
            &table_grid,
            None,
            None,
            1280,
            720,
        );
        assert!(none.is_empty(), "an empty inventory draws no icons");

        let closed = inventory_icon_vertices(
            InventoryMode::Closed,
            &layout,
            &full,
            &player_grid,
            &table_grid,
            None,
            None,
            1280,
            720,
        );
        assert!(closed.is_empty(), "a closed screen draws no icons");
    }

    #[test]
    fn table_mode_adds_sixteen_cells_but_player_mode_does_not() {
        let inventory = Inventory::empty();
        let player_grid = CraftingGrid::new(GridKind::Player);
        let mut table_grid = CraftingGrid::new(GridKind::Table);
        for cell in 0..16 {
            table_grid
                .set(cell, Some(item(ItemId::Block(BlockId::Grass))))
                .expect("active table cell");
        }

        let player_layout = open_layout(InventoryMode::Player);
        let player_verts = inventory_icon_vertices(
            InventoryMode::Player,
            &player_layout,
            &inventory,
            &player_grid,
            &table_grid,
            None,
            None,
            1280,
            720,
        );
        assert!(player_verts.is_empty(), "player mode ignores table cells");

        let table_layout = open_layout(InventoryMode::Table);
        let table_verts = inventory_icon_vertices(
            InventoryMode::Table,
            &table_layout,
            &inventory,
            &player_grid,
            &table_grid,
            None,
            None,
            1280,
            720,
        );
        assert_eq!(table_verts.len(), 16 * 18);
    }

    #[test]
    fn result_previews_and_cursor_ghost_emit_block_cubes() {
        let inventory = Inventory::empty();
        let mut player_grid = CraftingGrid::new(GridKind::Player);
        // One oak log -> planks preview, whose output is a block.
        player_grid
            .set(0, Some(item(ItemId::Block(BlockId::OakLog))))
            .expect("player cell");
        let table_grid = CraftingGrid::new(GridKind::Table);
        let layout = open_layout(InventoryMode::Player);

        let with_preview = inventory_icon_vertices(
            InventoryMode::Player,
            &layout,
            &inventory,
            &player_grid,
            &table_grid,
            None,
            None,
            1280,
            720,
        );
        // One block ingredient cube plus the block result preview cube.
        assert_eq!(with_preview.len(), 2 * 18, "player result preview cube");

        let mut no_recipe = CraftingGrid::new(GridKind::Player);
        no_recipe
            .set(0, Some(item(ItemId::Block(BlockId::Stone))))
            .expect("player cell");
        let cell_only = inventory_icon_vertices(
            InventoryMode::Player,
            &layout,
            &inventory,
            &no_recipe,
            &table_grid,
            None,
            None,
            1280,
            720,
        );
        assert_eq!(cell_only.len(), 18, "a lone stone cell is not a recipe");

        let cursor_inventory = Inventory::from_parts(
            [None; INVENTORY_SLOTS],
            Some(item(ItemId::Block(BlockId::Stone))),
        );
        let with_cursor = inventory_icon_vertices(
            InventoryMode::Player,
            &layout,
            &cursor_inventory,
            &CraftingGrid::new(GridKind::Player),
            &table_grid,
            cursor_inventory.cursor(),
            Some((640.0, 360.0)),
            1280,
            720,
        );
        assert_eq!(with_cursor.len(), 18, "cursor ghost cube");

        let no_cursor_pos = inventory_icon_vertices(
            InventoryMode::Player,
            &layout,
            &cursor_inventory,
            &CraftingGrid::new(GridKind::Player),
            &table_grid,
            cursor_inventory.cursor(),
            None,
            1280,
            720,
        );
        assert!(
            no_cursor_pos.is_empty(),
            "a block on the cursor without a finite position draws nothing"
        );

        let nan_cursor = inventory_icon_vertices(
            InventoryMode::Player,
            &layout,
            &cursor_inventory,
            &CraftingGrid::new(GridKind::Player),
            &table_grid,
            cursor_inventory.cursor(),
            Some((f32::NAN, 360.0)),
            1280,
            720,
        );
        assert!(
            nan_cursor.is_empty(),
            "non-finite cursor position is skipped"
        );
    }

    #[test]
    fn table_result_preview_emits_a_block_cube() {
        let inventory = Inventory::empty();
        let player_grid = CraftingGrid::new(GridKind::Player);
        let mut table_grid = CraftingGrid::new(GridKind::Table);
        for cell in [0, 1, 4, 5] {
            table_grid
                .set(cell, Some(item(ItemId::Block(BlockId::OakPlanks))))
                .expect("active table cell");
        }
        let layout = open_layout(InventoryMode::Table);
        let verts = inventory_icon_vertices(
            InventoryMode::Table,
            &layout,
            &inventory,
            &player_grid,
            &table_grid,
            None,
            None,
            1280,
            720,
        );
        // Four ingredient cubes plus the crafting-table block preview cube.
        assert_eq!(verts.len(), 5 * 18);
    }

    #[test]
    fn non_block_items_produce_no_s01_icons() {
        let stick = item(ItemId::Material(Material::Stick));
        let inventory = Inventory::from_parts([Some(stick); INVENTORY_SLOTS], None);
        let layout = open_layout(InventoryMode::Player);
        let verts = inventory_icon_vertices(
            InventoryMode::Player,
            &layout,
            &inventory,
            &CraftingGrid::new(GridKind::Player),
            &CraftingGrid::new(GridKind::Table),
            None,
            None,
            1280,
            720,
        );
        assert!(verts.is_empty(), "materials wait for the S02 icon path");

        let hotbar = hotbar_icon_vertices(&inventory, 1280, 720);
        assert!(hotbar.is_empty(), "materials draw no hotbar cube");
    }

    #[test]
    fn hotbar_icons_fill_the_nine_slots_and_stay_above_count_labels() {
        let inventory = block_inventory(INVENTORY_SLOTS);
        let verts = hotbar_icon_vertices(&inventory, 1280, 720);
        assert_eq!(verts.len(), HOTBAR_SLOTS * 18);

        let rects = hotbar_slot_rects(1280, 720);
        for (slot, chunk) in verts.chunks(18).enumerate() {
            let (x, y, w, h) = rects[slot];
            let label_top = super::super::hud::hotbar_count_text_origin(rects[slot]).1;
            for v in chunk {
                let (px, py) = px_vertex(v, 1280, 720);
                assert!(
                    px >= x - 1e-3 && px <= x + w + 1e-3,
                    "slot {slot} x in rect"
                );
                assert!(
                    py >= y - 1e-3 && py <= y + h + 1e-3,
                    "slot {slot} y in rect"
                );
                assert!(
                    py <= label_top + 1e-3,
                    "slot {slot} icon must not cover the count label (py {py} > {label_top})"
                );
            }
        }
    }

    #[test]
    fn hotbar_icons_respect_degenerate_surfaces() {
        let inventory = block_inventory(INVENTORY_SLOTS);
        for (sw, sh) in [(0, 0), (1, 1), (80, 60)] {
            let verts = hotbar_icon_vertices(&inventory, sw, sh);
            assert_eq!(verts.len(), HOTBAR_SLOTS * 18);
            assert!(verts
                .iter()
                .all(|v| v.clip_pos.iter().all(|c| c.is_finite())));
        }
    }

    #[test]
    fn main_uploads_icons_before_the_single_render_pass() {
        // Source pin: icons are built and uploaded after the quad upload but
        // before the UI pass opens, and the icon pass draws after quads.
        let src = include_str!("../main.rs");
        let set_quads_at = src.find("gpu.ui.set_quads(").expect("quad upload exists");
        let set_icons_at = src.find("gpu.ui.set_icons(").expect("icon upload exists");
        let render_at = src
            .find("let ui_pass: Option<&dyn UiPass>")
            .expect("render pass construction exists");
        assert!(
            set_quads_at < set_icons_at && set_icons_at < render_at,
            "main must upload icons after quads and before the render pass"
        );
        assert!(
            src.contains("hotbar_icon_vertices("),
            "main must draw the hotbar icons"
        );
        assert!(
            src.contains("inventory_icon_vertices("),
            "main must draw the inventory panel icons"
        );
    }

    #[test]
    fn icon_shader_samples_the_texture_array_with_a_per_vertex_layer() {
        assert!(
            ICON_SHADER_WGSL.contains("texture_2d_array<f32>"),
            "the icon pipeline must bind the world block texture array"
        );
        assert!(
            ICON_SHADER_WGSL.contains("textureSample(tex, samp, in.uv, i32(in.layer))"),
            "the fragment stage must sample the layer carried by the vertex"
        );
        assert!(
            ICON_SHADER_WGSL.contains("@interpolate(flat) layer: u32"),
            "the integer layer must be flat-interpolated"
        );
    }

    #[test]
    fn ui_draw_order_is_quads_then_logo_then_icons_then_text() {
        // Source pin on UiRenderer::draw_ui: icons must sit above panel/tint
        // quads and the pack logo, but below glyphon text (counts/labels).
        let src = include_str!("mod.rs");
        let quads_at = src
            .find("self.quad_renderer.draw(pass)")
            .expect("quad draw exists");
        let logo_at = src
            .find("self.logo_renderer.draw(pass)")
            .expect("logo draw exists");
        let icons_at = src
            .find("self.icon_renderer.draw(pass)")
            .expect("icon draw exists");
        let text_at = src
            .find("self.text_renderer.render(&self.atlas, pass)")
            .expect("text draw exists");
        assert!(
            quads_at < logo_at && logo_at < icons_at && icons_at < text_at,
            "UI order must be quads -> logo -> icons -> text"
        );
    }

    #[test]
    fn empty_icon_batches_clear_the_gpu_buffer() {
        // Source pin: an empty slice must leave vertex_count 0 and no buffer,
        // so closing every inventory never uploads or draws anything.
        let src = include_str!("item_icon.rs");
        let set_icons_at = src.find("pub fn set_icons").expect("set_icons exists");
        let set_icons_block = &src[set_icons_at..];
        assert!(set_icons_block.contains("self.vertex_count = verts.len() as u32"));
        assert!(set_icons_block.contains("if verts.is_empty()"));
        assert!(set_icons_block.contains("None"));
    }

    #[test]
    fn sanitize_icons_drops_non_finite_vertices_and_accepts_empty() {
        assert!(sanitize_icon_vertices(&[], 1280, 720).is_empty());

        let good = IconVertex::new([0.0, 0.0, 0.0, 1.0], [0.0, 0.0], 3);
        let bad = IconVertex::new([f32::NAN, 0.0, 0.0, 1.0], [0.0, 0.0], 3);
        let kept = sanitize_icon_vertices(&[good, bad], 1280, 720);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].layer, 3);
    }

    #[test]
    fn zero_and_non_finite_surfaces_still_produce_finite_icon_vertices() {
        let layers = block_icon_layers(BlockId::Stone);
        for (sw, sh) in [(0, 0), (1, 1)] {
            let verts =
                cube_icon_vertices((f32::NAN, f32::NEG_INFINITY, 32.0, 32.0), layers, sw, sh);
            assert_eq!(verts.len(), 18);
            assert!(verts
                .iter()
                .all(|v| v.clip_pos.iter().all(|c| c.is_finite())));
        }
    }

    #[test]
    fn main_wires_the_world_block_texture_into_the_ui_icon_renderer() {
        // Source pin: the UI is built after the renderer and handed the same
        // block texture view/sampler, so icons always match the active pack.
        let src = include_str!("../main.rs");
        let ui_at = src
            .find("let mut ui = UiRenderer::new(")
            .expect("UiRenderer::new call exists");
        let ui_block = &src[ui_at..];
        assert!(ui_block.contains("&renderer.block_texture_view"));
        assert!(ui_block.contains("&renderer.block_texture_sampler"));
        let renderer_at = src
            .find("let renderer = VoxelRenderer::new(&ctx);")
            .expect("renderer construction exists");
        assert!(
            renderer_at < ui_at,
            "the renderer must be constructed before the UI so its texture handles exist"
        );
    }

    // -----------------------------------------------------------------------
    // Procedural item sprite atlas and total icon mapping (S02-T01)
    // -----------------------------------------------------------------------

    const ALL_BLOCKS: [BlockId; 23] = [
        BlockId::Air,
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
        BlockId::Bedrock,
        BlockId::CoalOre,
        BlockId::IronOre,
        BlockId::CraftingTable,
        BlockId::Water,
        BlockId::Farmland,
        BlockId::WheatStage0,
        BlockId::WheatStage1,
        BlockId::WheatStage2,
        BlockId::WheatStage3,
        BlockId::Chest,
        BlockId::LootChest,
    ];

    const ALL_MATERIALS: [Material; 11] = [
        Material::Stick,
        Material::Apple,
        Material::Wheat,
        Material::WheatSeeds,
        Material::Bread,
        Material::Leather,
        Material::RawPorkchop,
        Material::RawBeef,
        Material::RottenFlesh,
        Material::Bone,
        Material::Arrow,
    ];

    const TIERED_KINDS: [ToolKind; 5] = [
        ToolKind::Pickaxe,
        ToolKind::Axe,
        ToolKind::Shovel,
        ToolKind::Hoe,
        ToolKind::Sword,
    ];

    const ALL_TIERS: [ToolTier; 3] = [ToolTier::Wood, ToolTier::Stone, ToolTier::Iron];

    fn all_tools() -> Vec<ItemId> {
        let mut ids = Vec::new();
        for kind in TIERED_KINDS {
            for tier in ALL_TIERS {
                ids.push(ItemId::Tool { kind, tier });
            }
        }
        ids.push(ItemId::Tool {
            kind: ToolKind::Bow,
            tier: ToolTier::Wood,
        });
        ids
    }

    fn stable_hash(bytes: &[u8]) -> u64 {
        // FNV-1a: tiny, deterministic, and stable across platforms.
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    #[test]
    fn every_carryable_item_has_exactly_one_icon_source() {
        let mut carryable_blocks = 0;
        for block in ALL_BLOCKS {
            let id = ItemId::Block(block);
            match icon_source(id) {
                Some(IconSource::Block(layers)) => {
                    assert!(block.is_carryable(), "{block:?} is not carryable");
                    assert_eq!(layers, block_icon_layers(block));
                    assert_eq!(item_sprite_layer(id), None, "blocks never use sprites");
                    carryable_blocks += 1;
                }
                Some(IconSource::Sprite(_)) => {
                    panic!("block {block:?} must use the block texture array")
                }
                None => assert!(
                    !block.is_carryable(),
                    "carryable block {block:?} has no icon source"
                ),
            }
        }
        let expected_carryable = ALL_BLOCKS.iter().filter(|b| b.is_carryable()).count();
        assert_eq!(carryable_blocks, expected_carryable);

        for material in ALL_MATERIALS {
            let id = ItemId::Material(material);
            match icon_source(id) {
                Some(IconSource::Sprite(layer)) => {
                    assert_eq!(item_sprite_layer(id), Some(layer))
                }
                other => panic!("material {material:?} icon source {other:?}"),
            }
        }

        for id in all_tools() {
            match icon_source(id) {
                Some(IconSource::Sprite(layer)) => {
                    assert_eq!(item_sprite_layer(id), Some(layer))
                }
                other => panic!("tool {id:?} icon source {other:?}"),
            }
        }

        // Unconstructible tool pairs are unrepresentable as icons too.
        for tier in [ToolTier::Stone, ToolTier::Iron] {
            let id = ItemId::Tool {
                kind: ToolKind::Bow,
                tier,
            };
            assert_eq!(item_sprite_layer(id), None, "{tier:?} bow must not exist");
            assert_eq!(icon_source(id), None, "{tier:?} bow must not exist");
        }
    }

    #[test]
    fn sprite_atlas_has_32_layers_of_16x16_rgba() {
        let layers = generate_item_sprite_layers_rgba();
        assert_eq!(layers.len(), ITEM_SPRITE_LAYERS as usize);
        for (index, layer) in layers.iter().enumerate() {
            assert_eq!(
                layer.len(),
                ITEM_SPRITE_BYTES,
                "layer {index} must be 16*16*4 bytes"
            );
        }
    }

    #[test]
    fn used_sprite_layers_are_opaque_deterministic_and_pairwise_distinct() {
        let first = generate_item_sprite_layers_rgba();
        let second = generate_item_sprite_layers_rgba();
        assert_eq!(first, second, "sprite generation must be deterministic");

        let mut used = Vec::new();
        for (_, layer) in ITEM_SPRITE_TABLE {
            used.push(layer);
        }
        assert_eq!(used.len(), 27, "the pinned table has 27 identities");

        let mut hashes = std::collections::HashSet::new();
        for layer in &used {
            let bytes = &first[*layer as usize];
            let opaque = bytes.chunks_exact(4).filter(|pixel| pixel[3] > 0).count();
            assert!(
                opaque >= 24,
                "layer {layer} must be a real sprite, only {opaque} opaque pixels"
            );
            assert!(
                hashes.insert(stable_hash(bytes)),
                "layer {layer} duplicates another sprite"
            );
        }
        assert_eq!(hashes.len(), 27, "every used sprite must be distinct");

        for layer in 27..ITEM_SPRITE_LAYERS {
            assert!(
                first[layer as usize].iter().all(|byte| *byte == 0),
                "reserved layer {layer} must stay fully transparent"
            );
        }
    }

    #[test]
    fn each_tool_tier_sprite_differs_from_its_siblings() {
        let layers = generate_item_sprite_layers_rgba();
        let sprite = |kind: ToolKind, tier: ToolTier| {
            let id = ItemId::Tool { kind, tier };
            let layer = item_sprite_layer(id).expect("constructible tool has a sprite");
            &layers[layer as usize]
        };

        for kind in TIERED_KINDS {
            let wood = sprite(kind, ToolTier::Wood);
            let stone = sprite(kind, ToolTier::Stone);
            let iron = sprite(kind, ToolTier::Iron);
            assert_ne!(wood, stone, "{kind:?} wood and stone must differ");
            assert_ne!(stone, iron, "{kind:?} stone and iron must differ");
            assert_ne!(wood, iron, "{kind:?} wood and iron must differ");
        }

        // The wood-only bow must not alias any tiered tool sprite.
        let bow = sprite(ToolKind::Bow, ToolTier::Wood);
        for kind in TIERED_KINDS {
            assert_ne!(bow, sprite(kind, ToolTier::Wood), "bow must be unique");
        }
    }

    #[test]
    fn sprite_layer_indices_are_pinned_and_append_only() {
        for (index, material) in ALL_MATERIALS.iter().enumerate() {
            assert_eq!(
                item_sprite_layer(ItemId::Material(*material)),
                Some(index as u32),
                "material {material:?} index moved"
            );
        }

        assert_eq!(
            item_sprite_layer(ItemId::Material(Material::Stick)),
            Some(0)
        );
        assert_eq!(
            item_sprite_layer(ItemId::Material(Material::Arrow)),
            Some(10)
        );

        for (kind_index, kind) in TIERED_KINDS.iter().enumerate() {
            for (tier_index, tier) in ALL_TIERS.iter().enumerate() {
                let expected = 11 + kind_index as u32 * 3 + tier_index as u32;
                assert_eq!(
                    item_sprite_layer(ItemId::Tool {
                        kind: *kind,
                        tier: *tier
                    }),
                    Some(expected),
                    "{kind:?} {tier:?} index moved"
                );
            }
        }

        assert_eq!(
            item_sprite_layer(ItemId::Tool {
                kind: ToolKind::Bow,
                tier: ToolTier::Wood
            }),
            Some(26),
            "the bow is pinned to the final used layer"
        );
    }
}
