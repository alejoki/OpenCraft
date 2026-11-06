//! Item icon geometry, source mapping, and the UI icon renderer.
//!
//! Two icon sources feed one pipeline: carryable blocks render as three-face
//! cubes sampled from the world block texture array, while materials and tools
//! render as flat quads sampled from a UI-only procedural sprite array
//! ([`generate_item_sprite_layers_rgba`]). Every carryable [`ItemId`] is
//! classified by [`icon_source`], so a slot can never silently stay empty.
//!
//! Everything above the renderer is GPU-free: given a pixel-space slot rect it
//! emits textured triangles in clip space, exactly like `ui::quad` and
//! `ui::logo` do for their own vertices. Keeping this module's geometry pure
//! means cubes and sprite quads can be proven headless, before any wgpu device
//! exists.
//!
//! Coordinates are physical pixels, top-left origin, y down, converted to NDC
//! with the codebase-wide convention (`x_ndc = 2x/sw - 1`,
//! `y_ndc = 1 - 2y/sh`, surface dimensions guarded with `.max(1)`).

use std::collections::BTreeMap;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use super::hud::{hotbar_icon_rect, hotbar_slot_rects};
use super::inventory::{InventoryLayout, InventoryMode, Rect};
use super::palette::{self, PaletteLayout, PaletteScreen};
use crate::content::{ContentRegistry, PACK_ICON_BASE, PACK_ICON_CAP};
use crate::player::inventory::{
    CraftingGrid, Inventory, Item, ItemId, Material, RecipePreview, ToolKind, ToolTier,
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
///
/// This is the authored/procedural contract (layers `0..32`); it never grows.
/// The GPU array itself is [`ITEM_SPRITE_ARRAY_LAYERS`], with pack icons laid
/// out above this base so pre-M010 icon bytes stay byte-identical.
pub const ITEM_SPRITE_LAYERS: u32 = 32;

/// GPU depth of the UI item sprite texture array.
///
/// `0..32` are the procedural layers, `32..=223` are registry pack icons (see
/// [`crate::content::PACK_ICON_BASE`] / [`crate::content::PACK_ICON_CAP`]), and
/// `224..256` stay transparent padding. The shader, pipeline and vertex layout
/// are unchanged: only the array depth and the per-vertex layer numbers grow.
pub const ITEM_SPRITE_ARRAY_LAYERS: u32 = 256;

// The atlas layout is a fixed contract: procedural layers fill `0..32`, pack
// icons start exactly at `PACK_ICON_BASE` and must fit inside the GPU depth.
const _: () = assert!(PACK_ICON_BASE as u32 == ITEM_SPRITE_LAYERS);
const _: () = assert!(ITEM_SPRITE_LAYERS <= ITEM_SPRITE_ARRAY_LAYERS);
const _: () = assert!(PACK_ICON_CAP as u32 <= ITEM_SPRITE_ARRAY_LAYERS);

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
    if let ItemId::Item(_) = id {
        return Some(u32::from(crate::content::PLACEHOLDER_ICON_LAYER));
    }
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
        ItemId::Item(_) => Some(IconSource::Sprite(u32::from(
            crate::content::PLACEHOLDER_ICON_LAYER,
        ))),
        // A pack tool has no procedural art yet; the reserved placeholder
        // sprite keeps the slot visible until a registry icon path resolves.
        ItemId::ModTool(_) => Some(IconSource::Sprite(u32::from(
            crate::content::PLACEHOLDER_ICON_LAYER,
        ))),
    }
}

/// Registry-aware icon-source mapping.
///
/// Same contract as [`icon_source`], but pack-authored identities resolve
/// their frozen session definitions:
///
/// * carryable blocks use `BlockDef.faces` with top = `faces[2]`, left =
///   `faces[4]`, right = `faces[0]` — the same directions
///   [`block_icon_layers`] uses, so built-in blocks stay byte-identical.
/// * registry items use `ItemDef.icon_layer`, which the registry already
///   points at [`crate::content::PLACEHOLDER_ICON_LAYER`] for placeholder art.
///
/// A missing definition (forged or unknown id) falls back to the registry-free
/// mapping instead of panicking, so unknown content renders the reserved
/// checker/placeholder policy. Everything else delegates to [`icon_source`].
pub fn icon_source_with(registry: &ContentRegistry, id: ItemId) -> Option<IconSource> {
    match id {
        ItemId::Block(block) => match registry.block(block) {
            Some(def) if block.is_carryable() => Some(IconSource::Block(BlockIconLayers {
                top: def.faces[2],
                left: def.faces[4],
                right: def.faces[0],
            })),
            _ => icon_source(id),
        },
        ItemId::Item(index) => Some(IconSource::Sprite(u32::from(
            registry
                .item(index)
                .map_or(crate::content::PLACEHOLDER_ICON_LAYER, |def| def.icon_layer),
        ))),
        ItemId::ModTool(index) => Some(IconSource::Sprite(u32::from(
            registry
                .tool(index)
                .map_or(crate::content::PLACEHOLDER_ICON_LAYER, |def| def.icon_layer),
        ))),
        ItemId::Material(_) | ItemId::Tool { .. } => icon_source(id),
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
        // Layer 27 is the reserved transparent checker slot: a placeholder
        // pack item draws no art until a pack supplies an icon.
        ItemId::Item(_) => {}
        // Pack tool art is registry-authored; there is no procedural fallback.
        ItemId::ModTool(_) => {}
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

/// Resolve a built-in sprite override name to its pinned procedural layer.
///
/// Only the append-only [`ITEM_SPRITE_TABLE`] slots (`0..=26`) are overridable:
/// the reserved placeholder layer 27 and the spare layers `28..=31` can never
/// be replaced. Blocks, forged tool pairs, and unknown names return `None`.
pub fn builtin_icon_layer(name: &str) -> Option<u32> {
    let id = crate::content::names::builtin_item_sprite_id_by_name(name)?;
    let layer = item_sprite_layer(id)?;
    (layer < ITEM_SPRITE_TABLE.len() as u32).then_some(layer)
}

/// Compose the full [`ITEM_SPRITE_ARRAY_LAYERS`] GPU sprite atlas once per
/// renderer from the frozen session registry's pack icons.
///
/// Layers `0..32` are exactly [`generate_item_sprite_layers_rgba`], so every
/// built-in icon stays byte-identical. Pack icons are copied to
/// `32 + index` in registry order, capped at [`crate::content::PACK_ICON_CAP`]
/// (layer 224). A malformed entry (not exactly [`ITEM_SPRITE_BYTES`]) becomes a
/// transparent slot rather than a short GPU upload; unused slots `224..256`
/// are transparent `[0, 0, 0, 0]` padding. The result is always exactly 256
/// layers, because a `texture_2d_array` needs a fixed depth.
pub fn compose_item_sprite_layers(pack_icons: &[Vec<u8>]) -> Vec<Vec<u8>> {
    compose_item_sprite_layers_with_overrides(pack_icons, &BTreeMap::new())
}

/// Compose the item sprite atlas with live built-in sprite overrides applied.
///
/// Starts from [`compose_item_sprite_layers`], then copies each override into
/// its pinned procedural layer (resolved by [`builtin_icon_layer`]). Unknown
/// names, malformed byte lengths, and non-overridable layers are ignored, so
/// a caller can pass presentation-owned visible state without validating it
/// again. An empty map produces exactly the plain composition; only layers
/// `0..=26` can ever change.
pub fn compose_item_sprite_layers_with_overrides(
    pack_icons: &[Vec<u8>],
    overrides: &BTreeMap<String, Vec<u8>>,
) -> Vec<Vec<u8>> {
    let mut layers = generate_item_sprite_layers_rgba();
    layers.resize(
        ITEM_SPRITE_ARRAY_LAYERS as usize,
        vec![0u8; ITEM_SPRITE_BYTES],
    );

    let copyable = (PACK_ICON_CAP - PACK_ICON_BASE) as usize;
    for (index, icon) in pack_icons.iter().take(copyable).enumerate() {
        let layer = usize::from(PACK_ICON_BASE) + index;
        if icon.len() == ITEM_SPRITE_BYTES {
            layers[layer] = icon.clone();
        }
    }

    for (name, bytes) in overrides {
        let Some(layer) = builtin_icon_layer(name) else {
            continue;
        };
        if bytes.len() != ITEM_SPRITE_BYTES {
            continue;
        }
        layers[layer as usize] = bytes.clone();
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

/// Axis-aligned textured quad for one procedural sprite layer inside `slot`.
///
/// Sprites are flat art, so unlike [`cube_icon_vertices`] they need no face
/// projection: one inset quad with full `0..=1` UVs and the sprite array layer
/// carried per vertex. The inset comes from [`icon_rect`], so sprite and cube
/// icons share one padding rule and both stay inside the slot.
pub fn sprite_icon_vertices(
    slot: Rect,
    layer: u32,
    surface_w: u32,
    surface_h: u32,
) -> Vec<IconVertex> {
    let (x, y, w, h) = icon_rect(slot);
    let sw = surface_w.max(1) as f32;
    let sh = surface_h.max(1) as f32;
    let mut verts = Vec::with_capacity(6);
    push_quad(
        &mut verts,
        [(x, y), (x + w, y), (x + w, y + h), (x, y + h)],
        layer,
        sw,
        sh,
    );
    verts
}

/// One frame's icon geometry, split by texture source.
///
/// The renderer owns one pipeline and one shader but two texture arrays (world
/// blocks and UI-only procedural sprites), so the CPU batch has to be split the
/// same way. Block cubes land in [`Self::block`] and sample the block array;
/// material/tool quads land in [`Self::sprite`] and sample the sprite array.
/// The split also fixes draw order: blocks first, sprites second.
#[derive(Clone, Debug, Default)]
pub struct IconDrawList {
    pub block: Vec<IconVertex>,
    pub sprite: Vec<IconVertex>,
    /// Frozen session registry used to resolve pack identities. `None` keeps
    /// the registry-free built-in mapping, so every pre-M010 caller and test
    /// stays byte-identical.
    registry: Option<Arc<ContentRegistry>>,
}

impl IconDrawList {
    pub fn new() -> Self {
        Self::default()
    }

    /// A batch that resolves pack block faces and item icons through `registry`.
    ///
    /// The `Arc` is cloned once per batch, never per item: the registry is the
    /// frozen session registry (MEM512), not a per-frame rebuild.
    pub fn with_registry(registry: Arc<ContentRegistry>) -> Self {
        Self {
            block: Vec::new(),
            sprite: Vec::new(),
            registry: Some(registry),
        }
    }

    /// Total vertices across both batches.
    pub fn len(&self) -> usize {
        self.block.len() + self.sprite.len()
    }

    /// True when neither batch would draw anything.
    pub fn is_empty(&self) -> bool {
        self.block.is_empty() && self.sprite.is_empty()
    }

    /// Classify one optional stack and append its icon geometry.
    ///
    /// Every carryable identity routes through [`icon_source`]: carryable
    /// blocks append an 18-vertex cube, materials and constructible tools
    /// append a 6-vertex sprite quad. Empty stacks, zero counts, and
    /// uncarryable/unconstructible identities append nothing, so a malformed
    /// caller can never draw a ghost icon.
    pub fn push_item(&mut self, item: Option<Item>, slot: Rect, surface_w: u32, surface_h: u32) {
        let Some(item) = item else {
            return;
        };
        if item.count() == 0 {
            return;
        }
        let Some(source) = self.registry.as_deref().map_or_else(
            || icon_source(item.id()),
            |registry| icon_source_with(registry, item.id()),
        ) else {
            return;
        };
        match source {
            IconSource::Block(layers) => self
                .block
                .extend(cube_icon_vertices(slot, layers, surface_w, surface_h)),
            IconSource::Sprite(layer) => self
                .sprite
                .extend(sprite_icon_vertices(slot, layer, surface_w, surface_h)),
        }
    }

    /// Append the icon for one recipe preview's output, if the output stack is
    /// valid and the identity has an icon source.
    pub fn push_preview(
        &mut self,
        preview: Option<RecipePreview>,
        slot: Rect,
        surface_w: u32,
        surface_h: u32,
    ) {
        let Some(preview) = preview else {
            return;
        };
        let item = Item::new(preview.output, preview.output_count).ok();
        self.push_item(item, slot, surface_w, surface_h);
    }

    /// Append another frame fragment's batches, preserving block-before-sprite
    /// ordering inside each batch.
    pub fn append(&mut self, mut other: IconDrawList) {
        self.block.append(&mut other.block);
        self.sprite.append(&mut other.sprite);
    }
}

/// All item icons for the open inventory panel.
///
/// Covers exactly the same surfaces as [`super::inventory::screen_quads`]:
/// 36 inventory slots, the 4 player crafting cells, the 16 table cells only
/// in [`InventoryMode::Table`], both result previews, and the cursor ghost.
/// Closed screens emit nothing. Block identities produce cubes, materials and
/// tools produce sprite quads.
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
) -> IconDrawList {
    inventory_icon_vertices_with(
        None,
        mode,
        layout,
        inventory,
        player_grid,
        table_grid,
        cursor_item,
        cursor_pos,
        surface_w,
        surface_h,
    )
}

/// [`inventory_icon_vertices`] with pack identities resolved through the frozen
/// session registry.
///
/// `None` is exactly the registry-free behavior; `Some` clones the `Arc` once
/// into the batch so each slot lookup is a plain table read.
#[allow(clippy::too_many_arguments)]
pub fn inventory_icon_vertices_with(
    registry: Option<&Arc<ContentRegistry>>,
    mode: InventoryMode,
    layout: &InventoryLayout,
    inventory: &Inventory,
    player_grid: &CraftingGrid,
    table_grid: &CraftingGrid,
    cursor_item: Option<Item>,
    cursor_pos: Option<(f32, f32)>,
    surface_w: u32,
    surface_h: u32,
) -> IconDrawList {
    let mut out = match registry {
        Some(registry) => IconDrawList::with_registry(Arc::clone(registry)),
        None => IconDrawList::new(),
    };
    if !mode.is_open() {
        return out;
    }

    for (index, rect) in layout.slots.iter().enumerate() {
        out.push_item(inventory.slot(index), *rect, surface_w, surface_h);
    }
    for (index, rect) in layout.player_grid.iter().enumerate() {
        out.push_item(player_grid.slot(index), *rect, surface_w, surface_h);
    }
    out.push_preview(
        player_grid.preview(),
        layout.player_result,
        surface_w,
        surface_h,
    );

    if mode == InventoryMode::Table {
        for (index, rect) in layout.table_grid.iter().enumerate() {
            out.push_item(table_grid.slot(index), *rect, surface_w, surface_h);
        }
        out.push_preview(
            table_grid.preview(),
            layout.table_result,
            surface_w,
            surface_h,
        );
    }

    if let (Some(item), Some((cx, cy))) = (cursor_item, cursor_pos) {
        if cx.is_finite() && cy.is_finite() {
            let size = layout.player_result.2 * 0.75;
            out.push_item(
                Some(item),
                (cx - size * 0.5, cy - size * 0.5, size, size),
                surface_w,
                surface_h,
            );
        }
    }

    out
}

/// Item icons for the nine gameplay hotbar slots.
///
/// Each icon is inset inside its [`hotbar_slot_rects`] slot and owns the full
/// slot height. Stack counts are drawn afterward as a bottom-left overlay with
/// a dark shadow, so the icon no longer shrinks to reserve a count strip.
/// Empty slots and non-carryable identities produce no vertices.
pub fn hotbar_icon_vertices(inventory: &Inventory, surface_w: u32, surface_h: u32) -> IconDrawList {
    hotbar_icon_vertices_with(None, inventory, surface_w, surface_h)
}

/// [`hotbar_icon_vertices`] with pack identities resolved through the frozen
/// session registry. `None` is exactly the registry-free behavior.
pub fn hotbar_icon_vertices_with(
    registry: Option<&Arc<ContentRegistry>>,
    inventory: &Inventory,
    surface_w: u32,
    surface_h: u32,
) -> IconDrawList {
    let mut out = match registry {
        Some(registry) => IconDrawList::with_registry(Arc::clone(registry)),
        None => IconDrawList::new(),
    };
    for (slot, rect) in hotbar_slot_rects(surface_w, surface_h).iter().enumerate() {
        let icon_slot = hotbar_icon_rect(*rect);
        out.push_item(inventory.hotbar(slot), icon_slot, surface_w, surface_h);
    }
    out
}

/// Item icons for the visible Creative palette grid.
///
/// `None` is exactly the registry-free behavior; `Some` clones the frozen
/// session registry once into the batch so pack blocks/items resolve through
/// their authored icons. A closed screen returns an empty batch, so the
/// palette can never change any pre-M010 pinned UI digest while it is closed.
/// Construction failures (a malformed identity) are ignored rather than
/// drawing a ghost icon.
pub fn palette_icon_vertices_with(
    registry: Option<&Arc<ContentRegistry>>,
    screen: &PaletteScreen,
    layout: &PaletteLayout,
    surface_w: u32,
    surface_h: u32,
) -> IconDrawList {
    let mut out = match registry {
        Some(registry) => IconDrawList::with_registry(Arc::clone(registry)),
        None => IconDrawList::new(),
    };
    if !screen.is_open() {
        return out;
    }

    for (visible, entry_index) in screen
        .filtered()
        .iter()
        .copied()
        .skip(screen.scroll())
        .take(palette::PALETTE_VISIBLE)
        .enumerate()
    {
        // `filtered()` stores indices into `entries`, not into `filtered`.
        // Resolve the entry directly; calling `filtered_entry` here would
        // double-index and drop or misdraw every icon after a search.
        let Some(entry) = screen.entries().get(entry_index) else {
            continue;
        };
        let item = match registry {
            Some(registry) => Item::new_with(registry, entry.id, 1).ok(),
            None => Item::new(entry.id, 1).ok(),
        };
        out.push_item(item, layout.slots[visible], surface_w, surface_h);
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

/// Texture format of the UI-only procedural sprite array.
///
/// Matches the block texture array (`Rgba8UnormSrgb`), so both bind groups feed
/// the same shader and produce identically gamma-correct icons.
const ITEM_SPRITE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Create the [`ITEM_SPRITE_ARRAY_LAYERS`]-layer item sprite texture array
/// and its sampler.
///
/// `layers` is the final composed atlas (see [`compose_item_sprite_layers`]),
/// uploaded exactly once at renderer construction: the array is never replaced
/// by a live pack change (MEM483/485/MEM512). `mip_level_count` is 1 because
/// 16x16 pixel art is sampled nearest and mips would only blur it. Entries that
/// are not exactly [`ITEM_SPRITE_BYTES`] are skipped rather than uploaded, so a
/// malformed caller cannot trip wgpu validation.
///
/// Returns only the view and sampler; a wgpu `TextureView` keeps its source
/// texture alive internally.
fn create_item_sprite_array(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layers: &[Vec<u8>],
) -> (wgpu::TextureView, wgpu::Sampler) {
    let size = wgpu::Extent3d {
        width: ITEM_SPRITE_SIZE,
        height: ITEM_SPRITE_SIZE,
        depth_or_array_layers: ITEM_SPRITE_ARRAY_LAYERS,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ui_item_sprite_array"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: ITEM_SPRITE_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    for (layer_index, layer) in layers
        .iter()
        .take(ITEM_SPRITE_ARRAY_LAYERS as usize)
        .enumerate()
    {
        if layer.len() != ITEM_SPRITE_BYTES {
            continue;
        }
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer_index as u32,
                },
                aspect: wgpu::TextureAspect::All,
            },
            layer,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(ITEM_SPRITE_SIZE * 4),
                rows_per_image: Some(ITEM_SPRITE_SIZE),
            },
            wgpu::Extent3d {
                width: ITEM_SPRITE_SIZE,
                height: ITEM_SPRITE_SIZE,
                depth_or_array_layers: 1,
            },
        );
    }

    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("ui_item_sprite_array_view"),
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        array_layer_count: Some(ITEM_SPRITE_ARRAY_LAYERS),
        ..Default::default()
    });

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("ui_item_sprite_sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });

    (view, sampler)
}

/// Draws textured item icons inside the color-only UI pass.
///
/// One pipeline and one shader serve two texture sources, each with its own
/// bind group and vertex buffer:
///
/// * `block` — three-face cubes sampling the world's block texture array (the
///   view/sampler borrowed at construction from
///   [`crate::render::pipeline::VoxelRenderer`]).
/// * `sprite` — flat material/tool quads sampling the UI-only procedural sprite
///   array, generated and uploaded internally in [`Self::new`].
///
/// Only the per-frame vertex buffers are rebuilt in [`set_icons`](Self::set_icons),
/// the same pattern as `QuadRenderer` and `LogoRenderer`.
///
/// Draw this strictly after the quad and logo renderers and strictly before
/// glyphon text, so icons sit above panels/tint and below labels/counts. Inside
/// [`Self::draw`], the block batch is issued before the sprite batch.
pub struct IconRenderer {
    pipeline: wgpu::RenderPipeline,
    /// Layout shared by the block and sprite bind groups. Kept so a session
    /// install can replace the sprite atlas without rebuilding the pipeline.
    bind_group_layout: wgpu::BindGroupLayout,
    block_bind_group: wgpu::BindGroup,
    sprite_bind_group: wgpu::BindGroup,
    block_vertex_buffer: Option<wgpu::Buffer>,
    block_vertex_count: u32,
    sprite_vertex_buffer: Option<wgpu::Buffer>,
    sprite_vertex_count: u32,
}

impl IconRenderer {
    pub fn new(
        ctx: &RenderContext,
        block_texture_view: &wgpu::TextureView,
        block_texture_sampler: &wgpu::Sampler,
    ) -> Self {
        Self::new_with_sprite_layers(
            ctx,
            block_texture_view,
            block_texture_sampler,
            &compose_item_sprite_layers(&[]),
        )
    }

    /// Construct the icon renderer with an explicit composed sprite atlas.
    ///
    /// `sprite_layers` is the final [`ITEM_SPRITE_ARRAY_LAYERS`]-layer atlas,
    /// normally [`compose_item_sprite_layers`] over the frozen session
    /// registry's pack icons. A changed pack stack applies on the next load;
    /// [`Self::set_sprite_layers`] replaces the atlas when that session is
    /// installed.
    pub fn new_with_sprite_layers(
        ctx: &RenderContext,
        block_texture_view: &wgpu::TextureView,
        block_texture_sampler: &wgpu::Sampler,
        sprite_layers: &[Vec<u8>],
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

        let block_bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui_item_icon_block_bind_group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(block_texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(block_texture_sampler),
                },
            ],
        });

        let (sprite_view, sprite_sampler) =
            create_item_sprite_array(&ctx.device, &ctx.queue, sprite_layers);
        let sprite_bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui_item_icon_sprite_bind_group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&sprite_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sprite_sampler),
                },
            ],
        });

        Self {
            pipeline,
            bind_group_layout,
            block_bind_group,
            sprite_bind_group,
            block_vertex_buffer: None,
            block_vertex_count: 0,
            sprite_vertex_buffer: None,
            sprite_vertex_count: 0,
        }
    }

    /// Replace the sprite atlas and its bind group in place.
    ///
    /// A session install can change the frozen content registry (for example
    /// loading a save whose pack authored item icons). The pipeline, block
    /// bind group and per-frame vertex buffers are untouched; only the UI-only
    /// sprite texture and its bind group are recreated.
    pub fn set_sprite_layers(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layers: &[Vec<u8>],
    ) {
        let (sprite_view, sprite_sampler) = create_item_sprite_array(device, queue, layers);
        self.sprite_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui_item_icon_sprite_bind_group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&sprite_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sprite_sampler),
                },
            ],
        });
    }

    /// Rebuild both per-frame icon vertex buffers. Pure CPU work (no texture
    /// upload); call once per frame before the UI pass opens. An empty slice
    /// clears that batch so nothing is drawn for it.
    pub fn set_icons(
        &mut self,
        device: &wgpu::Device,
        block_icons: &[IconVertex],
        sprite_icons: &[IconVertex],
        surface_w: u32,
        surface_h: u32,
    ) {
        let (block_buffer, block_count) = upload_icon_batch(
            device,
            "ui_item_icon_block_vbuf",
            block_icons,
            surface_w,
            surface_h,
        );
        self.block_vertex_buffer = block_buffer;
        self.block_vertex_count = block_count;

        let (sprite_buffer, sprite_count) = upload_icon_batch(
            device,
            "ui_item_icon_sprite_vbuf",
            sprite_icons,
            surface_w,
            surface_h,
        );
        self.sprite_vertex_buffer = sprite_buffer;
        self.sprite_vertex_count = sprite_count;
    }

    /// Draw pending icons into the UI pass: block cubes first, then sprites.
    /// No-op when nothing is queued.
    pub fn draw<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        if let (Some(buffer), true) = (&self.block_vertex_buffer, self.block_vertex_count > 0) {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.block_bind_group, &[]);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..self.block_vertex_count, 0..1);
        }
        if let (Some(buffer), true) = (&self.sprite_vertex_buffer, self.sprite_vertex_count > 0) {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.sprite_bind_group, &[]);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..self.sprite_vertex_count, 0..1);
        }
    }
}

/// Sanitize and upload one icon batch, returning its buffer and vertex count.
///
/// An empty (or fully sanitized-away) batch allocates no buffer at all, so
/// closing every screen never creates GPU resources.
fn upload_icon_batch(
    device: &wgpu::Device,
    label: &str,
    icons: &[IconVertex],
    surface_w: u32,
    surface_h: u32,
) -> (Option<wgpu::Buffer>, u32) {
    let verts = sanitize_icon_vertices(icons, surface_w, surface_h);
    if verts.is_empty() {
        return (None, 0);
    }
    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });
    (Some(buffer), verts.len() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::inventory::{
        CraftingGrid, GridKind, Inventory, Item, ItemId, Material, ToolKind, ToolTier,
        HOTBAR_SLOTS, INVENTORY_SLOTS,
    };
    use crate::ui::text_input::TextInputAction;

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
        assert_eq!((table.top, table.left, table.right), (23, 24, 24));
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
    fn push_item_classifies_blocks_and_sprites_and_rejects_invalid() {
        let mut grass = IconDrawList::new();
        grass.push_item(Some(item(ItemId::Block(BlockId::Grass))), SLOT, 800, 600);
        assert_eq!(grass.block.len(), 18, "carryable block -> cube");
        assert!(grass.sprite.is_empty());

        let mut stick = IconDrawList::new();
        stick.push_item(
            Some(item(ItemId::Material(Material::Stick))),
            SLOT,
            800,
            600,
        );
        assert!(stick.block.is_empty());
        assert_eq!(stick.sprite.len(), 6, "material -> one sprite quad");
        assert!(stick.sprite.iter().all(|v| v.layer == 0));

        let mut pickaxe = IconDrawList::new();
        pickaxe.push_item(
            Some(item(ItemId::Tool {
                kind: ToolKind::Pickaxe,
                tier: ToolTier::Wood,
            })),
            SLOT,
            800,
            600,
        );
        assert!(pickaxe.block.is_empty());
        assert_eq!(pickaxe.sprite.len(), 6, "tool -> one sprite quad");
        assert_eq!(pickaxe.sprite[0].layer, 11);

        // Uncarryable blocks and unconstructible tool pairs cannot even be
        // constructed as items, and they also have no icon source.
        for id in [
            ItemId::Block(BlockId::Air),
            ItemId::Block(BlockId::Water),
            ItemId::Tool {
                kind: ToolKind::Bow,
                tier: ToolTier::Stone,
            },
        ] {
            assert!(Item::new(id, 1).is_err(), "{id:?} is not constructible");
            assert!(icon_source(id).is_none(), "{id:?} has no icon source");
            let mut list = IconDrawList::new();
            list.push_item(Item::new(id, 1).ok(), SLOT, 800, 600);
            assert!(list.is_empty(), "{id:?} must not draw an icon");
        }

        // Empty stacks and zero counts add nothing either.
        let mut empty = IconDrawList::new();
        empty.push_item(None, SLOT, 800, 600);
        assert!(empty.is_empty());
        let zero = Item::new(ItemId::Material(Material::Apple), 0);
        assert!(zero.is_err(), "zero-count stacks are unrepresentable");
    }

    #[test]
    fn sprite_quads_are_unit_uv_and_inset_inside_the_slot() {
        let (sx, sy, sw, sh) = SLOT;
        let verts = sprite_icon_vertices(SLOT, 3, 800, 600);
        assert_eq!(verts.len(), 6);
        assert!(verts.iter().all(|v| v.layer == 3));
        let mut uvs = verts.iter().map(|v| (v.uv[0], v.uv[1])).collect::<Vec<_>>();
        uvs.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        uvs.dedup();
        assert_eq!(
            uvs,
            vec![(0.0, 0.0), (0.0, 1.0), (1.0, 0.0), (1.0, 1.0)],
            "the quad spans the full 0..=1 UV range"
        );

        let (ix, iy, iw, ih) = icon_rect(SLOT);
        for v in &verts {
            let (px, py) = px_vertex(v, 800, 600);
            assert!(px >= sx && px <= sx + sw, "sprite x inside slot");
            assert!(py >= sy && py <= sy + sh, "sprite y inside slot");
            assert!(px >= ix - 1e-3 && px <= ix + iw + 1e-3);
            assert!(py >= iy - 1e-3 && py <= iy + ih + 1e-3);
        }
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
        assert_eq!(verts.block.len(), INVENTORY_SLOTS * 18);
        assert!(verts.sprite.is_empty());

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
        assert_eq!(table_verts.block.len(), 16 * 18);
        assert!(table_verts.sprite.is_empty());
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
        assert_eq!(
            with_preview.block.len(),
            2 * 18,
            "player result preview cube"
        );
        assert!(with_preview.sprite.is_empty());

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
        assert_eq!(
            cell_only.block.len(),
            18,
            "a lone stone cell is not a recipe"
        );

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
        assert_eq!(with_cursor.block.len(), 18, "cursor ghost cube");

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
        assert_eq!(verts.block.len(), 5 * 18);
        assert!(verts.sprite.is_empty());
    }

    #[test]
    fn material_and_tool_inventories_render_only_sprite_quads() {
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
        assert!(
            verts.block.is_empty(),
            "materials never use the block array"
        );
        assert_eq!(verts.sprite.len(), INVENTORY_SLOTS * 6);
        assert!(verts.sprite.iter().all(|v| v.layer == 0));

        let hotbar = hotbar_icon_vertices(&inventory, 1280, 720);
        assert!(hotbar.block.is_empty());
        assert_eq!(hotbar.sprite.len(), HOTBAR_SLOTS * 6);
    }

    #[test]
    fn hotbar_icons_fill_the_nine_slots_and_counts_overlay_them() {
        let inventory = block_inventory(INVENTORY_SLOTS);
        let verts = hotbar_icon_vertices(&inventory, 1280, 720);
        assert_eq!(verts.block.len(), HOTBAR_SLOTS * 18);
        assert!(verts.sprite.is_empty());

        let rects = hotbar_slot_rects(1280, 720);
        for (slot, chunk) in verts.block.chunks(18).enumerate() {
            let (x, y, w, h) = rects[slot];
            let label_top = super::super::hud::hotbar_count_text_origin(rects[slot]).1;
            let mut max_py = f32::NEG_INFINITY;
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
                max_py = max_py.max(py);
            }
            assert!(
                max_py > label_top,
                "slot {slot} icon must use the full slot height, not stop at the count strip"
            );
        }
    }

    #[test]
    fn hotbar_icons_respect_degenerate_surfaces() {
        let inventory = block_inventory(INVENTORY_SLOTS);
        for (sw, sh) in [(0, 0), (1, 1), (80, 60)] {
            let verts = hotbar_icon_vertices(&inventory, sw, sh);
            assert_eq!(verts.block.len(), HOTBAR_SLOTS * 18);
            assert!(verts
                .block
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
            src.contains("hotbar_icon_vertices_with("),
            "main must draw the hotbar icons through the registry-aware builder"
        );
        assert!(
            src.contains("inventory_icon_vertices_with("),
            "main must draw the inventory panel icons through the registry-aware builder"
        );
        assert!(
            src.contains("Some(&self.session_registry)"),
            "icons must resolve pack content through the frozen session registry"
        );
        assert!(
            src.contains("IconDrawList"),
            "main must build one split icon draw list per frame"
        );
        assert!(
            src.contains("&icons.block") && src.contains("&icons.sprite"),
            "main must pass both batches to set_icons"
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
    fn empty_icon_batches_clear_the_gpu_buffers() {
        // Source pin: an empty batch must leave its count 0 and allocate no
        // buffer, so closing every inventory never uploads or draws anything.
        let src = include_str!("item_icon.rs");
        let set_icons_at = src.find("pub fn set_icons").expect("set_icons exists");
        let set_icons_block = &src[set_icons_at..];
        assert!(set_icons_block.contains("self.block_vertex_count = block_count"));
        assert!(set_icons_block.contains("self.sprite_vertex_count = sprite_count"));
        assert!(set_icons_block.contains("upload_icon_batch"));

        let helper_at = src
            .find("fn upload_icon_batch")
            .expect("upload helper exists");
        let helper = &src[helper_at..];
        assert!(helper.contains("if verts.is_empty()"));
        assert!(helper.contains("return (None, 0)"));
    }

    #[test]
    fn draw_issues_block_batch_before_sprite_batch() {
        // Source pin on IconRenderer::draw: the block batch is set first, then
        // the sprite batch, using one pipeline with two bind groups.
        let src = include_str!("item_icon.rs");
        let draw_at = src
            .find("pub fn draw<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>)")
            .expect("draw exists");
        let draw_block = &src[draw_at..];
        let end = draw_block
            .find("fn upload_icon_batch")
            .expect("helper follows draw");
        let draw_body = &draw_block[..end];
        let block_at = draw_body
            .find("&self.block_bind_group")
            .expect("block bind group draw");
        let sprite_at = draw_body
            .find("&self.sprite_bind_group")
            .expect("sprite bind group draw");
        assert!(
            block_at < sprite_at,
            "block cubes must draw before material/tool sprites"
        );
        assert_eq!(
            draw_body
                .matches("pass.set_pipeline(&self.pipeline)")
                .count(),
            2,
            "one pipeline serves both batches"
        );
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
            let (pixels, remainder) = bytes.as_chunks::<4>();
            assert!(remainder.is_empty(), "layer {layer} must be whole RGBA");
            let opaque = pixels.iter().filter(|pixel| pixel[3] > 0).count();
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

    #[test]
    fn placeholder_pack_items_use_the_reserved_transparent_layer() {
        let id = ItemId::Item(0);
        assert_eq!(
            item_sprite_layer(id),
            Some(u32::from(crate::content::PLACEHOLDER_ICON_LAYER))
        );
        assert_eq!(
            icon_source(id),
            Some(IconSource::Sprite(u32::from(
                crate::content::PLACEHOLDER_ICON_LAYER
            )))
        );

        // Layer 27 stays transparent: a placeholder item has no art until a
        // pack supplies one, and must not alias another material or tool.
        let layers = generate_item_sprite_layers_rgba();
        assert!(
            layers[usize::from(crate::content::PLACEHOLDER_ICON_LAYER)]
                .iter()
                .all(|byte| *byte == 0),
            "the reserved placeholder layer must stay fully transparent"
        );
    }

    // -----------------------------------------------------------------------
    // Sprite batches alongside block cubes (S02-T02)
    // -----------------------------------------------------------------------

    /// Every material and constructible tool, one per slot, cycling.
    fn sprite_inventory() -> Inventory {
        let mut ids: Vec<ItemId> = ALL_MATERIALS
            .iter()
            .map(|material| ItemId::Material(*material))
            .collect();
        ids.extend(all_tools());
        let mut slots = [None; INVENTORY_SLOTS];
        for (slot, id) in slots.iter_mut().zip(ids.iter().cycle()) {
            *slot = Some(item(*id));
        }
        Inventory::from_parts(slots, None)
    }

    #[test]
    fn full_material_inventory_yields_36_sprite_quads_and_no_block_vertices() {
        let inventory = sprite_inventory();
        let layout = open_layout(InventoryMode::Player);
        let icons = inventory_icon_vertices(
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
        assert!(
            icons.block.is_empty(),
            "materials/tools never sample the block texture array"
        );
        assert_eq!(icons.sprite.len(), INVENTORY_SLOTS * 6);
        assert_eq!(icons.sprite.len(), 216, "36 sprite quads");
        assert_eq!(icons.len(), 216);
    }

    #[test]
    fn mixed_inventory_splits_block_and_sprite_batches_exactly() {
        let mut slots = [None; INVENTORY_SLOTS];
        for (index, slot) in slots.iter_mut().enumerate() {
            *slot = Some(if index % 2 == 0 {
                item(ItemId::Block(BlockId::Stone))
            } else {
                item(ItemId::Material(Material::Apple))
            });
        }
        let inventory = Inventory::from_parts(slots, None);
        let layout = open_layout(InventoryMode::Player);
        let icons = inventory_icon_vertices(
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
        assert_eq!(icons.block.len(), 18 * 18, "18 block cubes");
        assert_eq!(icons.sprite.len(), 18 * 6, "18 sprite quads");
        assert_eq!(icons.len(), 18 * 18 + 18 * 6);
    }

    #[test]
    fn hotbar_apple_and_pickaxe_yield_two_sprite_quads() {
        let mut slots = [None; INVENTORY_SLOTS];
        slots[0] = Some(item(ItemId::Material(Material::Apple)));
        slots[1] = Some(item(ItemId::Tool {
            kind: ToolKind::Pickaxe,
            tier: ToolTier::Iron,
        }));
        let inventory = Inventory::from_parts(slots, None);
        let icons = hotbar_icon_vertices(&inventory, 1280, 720);
        assert!(icons.block.is_empty());
        assert_eq!(icons.sprite.len(), 2 * 6, "apple + pickaxe quads");

        // The apple uses layer 1 and the iron pickaxe layer 13; the batches
        // stay in hotbar slot order.
        assert!(icons.sprite[..6].iter().all(|v| v.layer == 1));
        assert!(icons.sprite[6..].iter().all(|v| v.layer == 13));
    }

    #[test]
    fn every_sprite_quad_stays_inside_its_slot_and_may_overlay_the_count_origin() {
        let inventory = sprite_inventory();
        let icons = hotbar_icon_vertices(&inventory, 1280, 720);
        assert_eq!(icons.sprite.len(), HOTBAR_SLOTS * 6);
        let rects = hotbar_slot_rects(1280, 720);
        for (slot, chunk) in icons.sprite.chunks(6).enumerate() {
            let (x, y, w, h) = rects[slot];
            let label_top = super::super::hud::hotbar_count_text_origin(rects[slot]).1;
            let mut max_py = f32::NEG_INFINITY;
            for v in chunk {
                let (px, py) = px_vertex(v, 1280, 720);
                assert!(
                    px >= x - 1e-3 && px <= x + w + 1e-3,
                    "slot {slot} sprite x {px} outside [{x}, {}]",
                    x + w
                );
                assert!(
                    py >= y - 1e-3 && py <= y + h + 1e-3,
                    "slot {slot} sprite y {py} outside [{y}, {}]",
                    y + h
                );
                max_py = max_py.max(py);
            }
            assert!(
                max_py > label_top,
                "slot {slot} sprite must use the full slot height, with the count drawn over it"
            );
        }
    }

    #[test]
    fn icon_source_covers_every_carryable_item_so_no_slot_stays_empty() {
        let mut carryable = Vec::new();
        for block in ALL_BLOCKS {
            let id = ItemId::Block(block);
            if block.is_carryable() {
                carryable.push(id);
            }
        }
        carryable.extend(ALL_MATERIALS.iter().map(|m| ItemId::Material(*m)));
        carryable.extend(all_tools());

        for id in carryable {
            let mut icons = IconDrawList::new();
            icons.push_item(Some(item(id)), SLOT, 800, 600);
            assert!(
                icons.len() == 18 || icons.len() == 6,
                "{id:?} must produce exactly one icon (got {} vertices)",
                icons.len()
            );
            assert_eq!(
                icons.block.is_empty(),
                matches!(icon_source(id), Some(IconSource::Sprite(_))),
                "{id:?} must land in the batch matching its icon source"
            );
        }
    }

    #[test]
    fn append_preserves_block_before_sprite_ordering() {
        let mut a = IconDrawList::new();
        a.push_item(Some(item(ItemId::Block(BlockId::Stone))), SLOT, 800, 600);
        let mut b = IconDrawList::new();
        b.push_item(
            Some(item(ItemId::Material(Material::Stick))),
            SLOT,
            800,
            600,
        );
        b.push_item(Some(item(ItemId::Block(BlockId::Dirt))), SLOT, 800, 600);
        a.append(b);
        assert_eq!(a.block.len(), 2 * 18);
        assert_eq!(a.sprite.len(), 6);
        // Both block cubes precede the sprite quad in their own batches, which
        // is what the two-draw renderer relies on.
        assert_eq!(a.len(), 2 * 18 + 6);
    }

    // -----------------------------------------------------------------------
    // Registry-aware icons and the 256-layer sprite atlas (M010/S04-T03)
    // -----------------------------------------------------------------------

    use crate::content::spec::{BlockEntry, BlockTextures, ItemEntry, PackAsset};
    use crate::content::{
        build_registry, PackContentSpec, PackIdentity, PACK_LAYER_BASE, PLACEHOLDER_BLOCK_LAYER,
        PLACEHOLDER_ICON_LAYER,
    };
    use crate::world::block::BUILTIN_BLOCK_COUNT;

    fn pack_asset(seed: u8) -> PackAsset {
        PackAsset {
            rgba: vec![seed; ITEM_SPRITE_BYTES],
            source_side: 16,
        }
    }

    /// One pack block (`mymod:ruby_ore`, distinct top/side art) and one pack
    /// item (`mymod:ruby`, its own icon), both resolved with zero errors.
    fn pack_registry() -> ContentRegistry {
        let mut spec = PackContentSpec::default();
        spec.assets
            .insert("ore_top.png".to_string(), pack_asset(0x11));
        spec.assets
            .insert("ore_side.png".to_string(), pack_asset(0x22));
        spec.assets
            .insert("ore_bottom.png".to_string(), pack_asset(0x44));
        spec.assets.insert("ruby.png".to_string(), pack_asset(0x33));
        spec.blocks.push(BlockEntry {
            id: "mymod:ruby_ore".to_string(),
            display_name: None,
            textures: BlockTextures {
                top: Some("ore_top.png".to_string()),
                bottom: Some("ore_bottom.png".to_string()),
                side: Some("ore_side.png".to_string()),
                ..BlockTextures::default()
            },
            hardness: 3.0,
            drop: None,
            tool: None,
            stack_limit: 64,
        });
        spec.items.push(ItemEntry {
            id: "mymod:ruby".to_string(),
            display_name: None,
            icon: "ruby.png".to_string(),
            stack_limit: 64,
            food: None,
        });

        let (registry, report) = build_registry(
            ContentRegistry::builtin(),
            &[(
                PackIdentity {
                    folder: "mymod".to_string(),
                    version: Some("1.0".to_string()),
                },
                spec,
            )],
        );
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        registry
    }

    #[test]
    fn composed_sprite_atlas_is_256_layers_and_keeps_the_procedural_base() {
        let composed = compose_item_sprite_layers(&[]);
        assert_eq!(composed.len(), ITEM_SPRITE_ARRAY_LAYERS as usize);
        assert_eq!(composed.len(), 256);
        assert!(
            composed
                .iter()
                .all(|layer| layer.len() == ITEM_SPRITE_BYTES),
            "every layer must be 16x16 RGBA"
        );

        let procedural = generate_item_sprite_layers_rgba();
        for (layer, (actual, expected)) in composed
            .iter()
            .take(ITEM_SPRITE_LAYERS as usize)
            .zip(procedural.iter())
            .enumerate()
        {
            assert_eq!(
                actual, expected,
                "built-in layer {layer} must stay byte-identical"
            );
        }
        for (layer, actual) in composed
            .iter()
            .enumerate()
            .take(ITEM_SPRITE_ARRAY_LAYERS as usize)
            .skip(ITEM_SPRITE_LAYERS as usize)
        {
            assert!(
                actual.iter().all(|byte| *byte == 0),
                "unused layer {layer} must stay transparent"
            );
        }
    }

    #[test]
    fn builtin_icon_layer_resolves_only_pinned_procedural_slots() {
        assert_eq!(builtin_icon_layer("apple"), Some(1));
        assert_eq!(builtin_icon_layer("wood_pickaxe"), Some(11));
        assert_eq!(builtin_icon_layer("wood_bow"), Some(26));

        // Blocks sample the world texture array, not this atlas.
        assert_eq!(builtin_icon_layer("stone"), None);
        // Forged tool pairs have no pinned slot.
        assert_eq!(builtin_icon_layer("stone_bow"), None);
        assert_eq!(builtin_icon_layer("iron_bow"), None);
        // Unknown names never resolve.
        assert_eq!(builtin_icon_layer("diamond"), None);
        assert_eq!(builtin_icon_layer(""), None);
    }

    #[test]
    fn overrides_replace_only_their_pinned_layer() {
        let override_bytes = vec![0xABu8; ITEM_SPRITE_BYTES];
        let mut overrides = BTreeMap::new();
        overrides.insert("apple".to_string(), override_bytes.clone());

        let composed = compose_item_sprite_layers_with_overrides(&[], &overrides);
        let base = compose_item_sprite_layers(&[]);

        assert_eq!(composed.len(), ITEM_SPRITE_ARRAY_LAYERS as usize);
        assert_eq!(composed[1], override_bytes);
        for layer in 0..ITEM_SPRITE_ARRAY_LAYERS as usize {
            if layer == 1 {
                continue;
            }
            assert_eq!(
                composed[layer], base[layer],
                "only the apple layer 1 may change, layer {layer} moved"
            );
        }
        // Spot-check the reserved and pack ranges explicitly.
        for layer in [0usize, 2, 26, 27, 31, 32, 224, 255] {
            assert_eq!(composed[layer], base[layer], "layer {layer}");
        }
    }

    #[test]
    fn malformed_and_unknown_overrides_are_ignored() {
        let base = compose_item_sprite_layers(&[]);
        let mut overrides = BTreeMap::new();
        overrides.insert("apple".to_string(), vec![0xFFu8; 100]);
        overrides.insert("diamond".to_string(), vec![0x11u8; ITEM_SPRITE_BYTES]);
        overrides.insert("stone".to_string(), vec![0x22u8; ITEM_SPRITE_BYTES]);

        let composed = compose_item_sprite_layers_with_overrides(&[], &overrides);
        assert_eq!(composed, base);
    }

    #[test]
    fn empty_overrides_match_plain_composition() {
        let icons = vec![vec![0x5Au8; ITEM_SPRITE_BYTES]];
        assert_eq!(
            compose_item_sprite_layers_with_overrides(&icons, &BTreeMap::new()),
            compose_item_sprite_layers(&icons)
        );
    }

    #[test]
    fn icon_source_with_apple_keeps_the_overridden_sprite_layer() {
        let registry = ContentRegistry::builtin();
        assert_eq!(
            icon_source_with(&registry, ItemId::Material(Material::Apple)),
            Some(IconSource::Sprite(1))
        );

        let override_bytes = vec![0x77u8; ITEM_SPRITE_BYTES];
        let mut overrides = BTreeMap::new();
        overrides.insert("apple".to_string(), override_bytes.clone());
        let composed = compose_item_sprite_layers_with_overrides(&[], &overrides);
        match icon_source_with(&registry, ItemId::Material(Material::Apple)) {
            Some(IconSource::Sprite(layer)) => {
                assert_eq!(composed[layer as usize], override_bytes);
            }
            other => panic!("apple icon source was {other:?}"),
        }
    }

    #[test]
    fn default_item_atlas_has_visible_art_for_every_builtin_item() {
        let layers = generate_item_sprite_layers_rgba();
        assert_eq!(layers.len(), ITEM_SPRITE_LAYERS as usize);
        for (id, layer) in ITEM_SPRITE_TABLE {
            assert!(
                layer < ITEM_SPRITE_LAYERS,
                "{id:?} must stay inside the procedural default range"
            );
            assert!(
                layers[layer as usize]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] != 0),
                "{id:?} must have visible default-pack art at layer {layer}"
            );
        }
        for layer in 27..ITEM_SPRITE_LAYERS {
            assert!(
                layers[layer as usize].iter().all(|byte| *byte == 0),
                "reserved default layer {layer} must stay transparent"
            );
        }

        // Selecting Default (no pack) composes exactly the procedural base,
        // so the built-in item textures never depend on a pack being enabled.
        let composed = compose_item_sprite_layers(&[]);
        assert_eq!(
            &composed[..ITEM_SPRITE_LAYERS as usize],
            layers.as_slice(),
            "the no-pack atlas must be the built-in default set"
        );
    }

    #[test]
    fn session_install_rebuilds_the_item_sprite_atlas_from_the_installed_registry() {
        let src = include_str!("../main.rs");
        let start = src
            .find("fn on_session_installed(&mut self)")
            .expect("on_session_installed must exist");
        let body = &src[start..];
        let end = body.find("\n    fn ").unwrap_or(body.len());
        let body = &body[..end];
        assert!(
            body.contains("compose_item_sprite_layers(self.session.registry.pack_icons())"),
            "a session install must compose from the installed frozen registry"
        );
        assert!(
            body.contains("set_item_sprite_layers"),
            "a session install must replace the UI sprite atlas"
        );
    }

    #[test]
    fn pack_icons_land_on_layer_32_and_the_rest_stay_transparent() {
        let icon0 = vec![0x5Au8; ITEM_SPRITE_BYTES];
        let icon1 = vec![0xA5u8; ITEM_SPRITE_BYTES];
        let composed = compose_item_sprite_layers(&[icon0.clone(), icon1.clone()]);

        assert_eq!(composed[PACK_ICON_BASE as usize], icon0);
        assert_eq!(composed[PACK_ICON_BASE as usize + 1], icon1);
        for (layer, actual) in composed
            .iter()
            .enumerate()
            .take(ITEM_SPRITE_ARRAY_LAYERS as usize)
            .skip(PACK_ICON_BASE as usize + 2)
        {
            assert!(
                actual.iter().all(|byte| *byte == 0),
                "layer {layer} must stay transparent"
            );
        }
    }

    #[test]
    fn oversize_pack_icon_list_caps_at_the_reserved_icon_cap() {
        let icons: Vec<Vec<u8>> = (0..200u16)
            .map(|index| vec![(index % 251) as u8; ITEM_SPRITE_BYTES])
            .collect();
        let composed = compose_item_sprite_layers(&icons);
        assert_eq!(composed.len(), ITEM_SPRITE_ARRAY_LAYERS as usize);

        let copyable = PACK_ICON_CAP as usize - PACK_ICON_BASE as usize;
        for (index, icon) in icons.iter().take(copyable).enumerate() {
            assert_eq!(composed[PACK_ICON_BASE as usize + index], *icon);
        }
        for (layer, actual) in composed
            .iter()
            .enumerate()
            .take(ITEM_SPRITE_ARRAY_LAYERS as usize)
            .skip(PACK_ICON_CAP as usize)
        {
            assert!(
                actual.iter().all(|byte| *byte == 0),
                "layer {layer} past the icon cap must stay transparent"
            );
        }
    }

    #[test]
    fn malformed_pack_icon_becomes_a_transparent_layer() {
        let composed = compose_item_sprite_layers(&[vec![0xFF; 100]]);
        assert_eq!(composed.len(), ITEM_SPRITE_ARRAY_LAYERS as usize);
        assert!(
            composed[PACK_ICON_BASE as usize]
                .iter()
                .all(|byte| *byte == 0),
            "a non-1024-byte icon must not reach the GPU atlas"
        );
    }

    #[test]
    fn icon_source_with_matches_icon_source_for_every_carryable_builtin() {
        let mut carryable = Vec::new();
        for block in ALL_BLOCKS {
            if block.is_carryable() {
                carryable.push(ItemId::Block(block));
            }
        }
        carryable.extend(
            ALL_MATERIALS
                .iter()
                .map(|material| ItemId::Material(*material)),
        );
        carryable.extend(all_tools());

        let registry = ContentRegistry::builtin();
        for id in carryable {
            assert_eq!(
                icon_source_with(registry, id),
                icon_source(id),
                "built-in icon mapping changed for {id:?}"
            );
        }
    }

    #[test]
    fn icon_source_with_resolves_pack_faces_and_item_icon_layer() {
        let registry = pack_registry();
        let block = BlockId::Modded(BUILTIN_BLOCK_COUNT);
        let def = registry.block(block).expect("pack block resolves");

        match icon_source_with(&registry, ItemId::Block(block)) {
            Some(IconSource::Block(layers)) => {
                assert_eq!(layers.top, def.faces[2], "top is +Y");
                assert_eq!(layers.left, def.faces[4], "left is +Z");
                assert_eq!(layers.right, def.faces[0], "right is +X");
                assert_ne!(
                    layers.top, layers.left,
                    "distinct top/side art proves the direction mapping"
                );
            }
            other => panic!("pack block icon source was {other:?}"),
        }

        let item = ItemId::Item(0);
        let icon_layer = registry.item(0).expect("pack item resolves").icon_layer;
        assert_eq!(icon_layer, PACK_ICON_BASE);
        assert_eq!(
            icon_source_with(&registry, item),
            Some(IconSource::Sprite(u32::from(icon_layer)))
        );

        // The composed atlas carries the authored icon bytes at that layer.
        let composed = compose_item_sprite_layers(registry.pack_icons());
        assert_eq!(
            composed[usize::from(icon_layer)],
            vec![0x33u8; ITEM_SPRITE_BYTES]
        );
    }

    #[test]
    fn icon_source_with_falls_back_to_reserved_placeholder_layers() {
        let mut registry = ContentRegistry::builtin().clone();
        let block = registry.append_placeholder_block("mymod:missing");
        let item = registry.append_placeholder_item("mymod:missing");

        let expected = BlockIconLayers {
            top: PLACEHOLDER_BLOCK_LAYER,
            left: PLACEHOLDER_BLOCK_LAYER,
            right: PLACEHOLDER_BLOCK_LAYER,
        };
        assert_eq!(
            icon_source_with(&registry, ItemId::Block(block)),
            Some(IconSource::Block(expected))
        );
        assert_eq!(
            icon_source_with(&registry, ItemId::Item(item)),
            Some(IconSource::Sprite(u32::from(PLACEHOLDER_ICON_LAYER)))
        );

        // Forged/unknown ids never panic and never alias real pack art.
        let unknown_block = ItemId::Block(BlockId::Modded(u16::MAX));
        assert_eq!(
            icon_source_with(&registry, unknown_block),
            icon_source(unknown_block)
        );
        assert_eq!(
            icon_source_with(&registry, ItemId::Item(u16::MAX)),
            Some(IconSource::Sprite(u32::from(PLACEHOLDER_ICON_LAYER)))
        );
    }

    #[test]
    fn draw_list_with_registry_emits_pack_layers_while_new_stays_builtin() {
        let registry = Arc::new(pack_registry());
        let block = BlockId::Modded(BUILTIN_BLOCK_COUNT);
        let def = registry.block(block).expect("pack block resolves");

        let mut with_block = IconDrawList::with_registry(Arc::clone(&registry));
        with_block.push_item(Some(item(ItemId::Block(block))), SLOT, 800, 600);
        assert_eq!(with_block.block.len(), 18);
        assert_eq!(with_block.block[0].layer, u32::from(def.faces[2]));
        assert_eq!(with_block.block[6].layer, u32::from(def.faces[4]));
        assert_eq!(with_block.block[12].layer, u32::from(def.faces[0]));
        assert_eq!(
            with_block.block[12].layer,
            u32::from(PACK_LAYER_BASE),
            "the first allocated pack face is PACK_LAYER_BASE"
        );

        // Without a registry the same pack item renders the reserved layer.
        let mut plain_item = IconDrawList::new();
        plain_item.push_item(Some(item(ItemId::Item(0))), SLOT, 800, 600);
        assert_eq!(plain_item.sprite.len(), 6);
        assert_eq!(
            plain_item.sprite[0].layer,
            u32::from(PLACEHOLDER_ICON_LAYER)
        );

        let mut with_item = IconDrawList::with_registry(registry);
        with_item.push_item(Some(item(ItemId::Item(0))), SLOT, 800, 600);
        assert_eq!(with_item.sprite.len(), 6);
        assert_eq!(with_item.sprite[0].layer, u32::from(PACK_ICON_BASE));
    }

    #[test]
    fn draw_list_registry_keeps_builtin_cube_layers_identical() {
        let registry = Arc::new(ContentRegistry::builtin().clone());
        let layers = |list: &IconDrawList| {
            list.block
                .iter()
                .map(|vertex| vertex.layer)
                .collect::<Vec<_>>()
        };

        let mut plain = IconDrawList::new();
        plain.push_item(Some(item(ItemId::Block(BlockId::OakLog))), SLOT, 800, 600);
        let mut with_registry = IconDrawList::with_registry(registry);
        with_registry.push_item(Some(item(ItemId::Block(BlockId::OakLog))), SLOT, 800, 600);
        assert_eq!(layers(&plain), layers(&with_registry));
    }

    // -----------------------------------------------------------------------
    // Creative palette icons (M010/S07-T03)
    // -----------------------------------------------------------------------

    /// The checked-in reference pack through the same production mapping as
    /// `main.rs::build_session_registry`.
    fn reference_registry() -> ContentRegistry {
        let spec = crate::pack::spec::load_spec_from_dir(std::path::Path::new(
            "tests/fixtures/packs/mymod-ruby",
        ));
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

    #[test]
    fn palette_icons_are_empty_while_the_screen_is_closed() {
        let registry = Arc::new(ContentRegistry::builtin().clone());
        let screen = PaletteScreen::new();
        let layout = palette::layout(1280, 720);

        assert!(
            palette_icon_vertices_with(Some(&registry), &screen, &layout, 1280, 720).is_empty()
        );
        assert!(palette_icon_vertices_with(None, &screen, &layout, 1280, 720).is_empty());
    }

    #[test]
    fn open_builtin_palette_emits_block_and_sprite_icons() {
        let registry = Arc::new(ContentRegistry::builtin().clone());
        let mut screen = PaletteScreen::new();
        screen.open(&registry);
        let layout = palette::layout(1280, 720);

        let icons = palette_icon_vertices_with(Some(&registry), &screen, &layout, 1280, 720);
        assert!(
            !icons.block.is_empty(),
            "built-in carryable blocks must draw cube icons"
        );
        assert!(
            !icons.sprite.is_empty(),
            "built-in materials/tools must draw sprite icons"
        );
        for vertex in icons.block.iter().chain(icons.sprite.iter()) {
            assert!(vertex.clip_pos.iter().all(|value| value.is_finite()));
        }
    }

    #[test]
    fn filtered_palette_icons_draw_exactly_the_matching_entries() {
        let registry = Arc::new(reference_registry());
        let mut screen = PaletteScreen::new();
        screen.open(&registry);
        let layout = palette::layout(1280, 720);

        // One authored block match: exactly one 18-vertex cube, no sprites.
        // Before the fix this resolved `filtered_entry(entry_index)` against
        // the one-element filtered list and silently drew nothing.
        screen.apply(TextInputAction::InsertText("mymod:ruby_ore".into()));
        assert_eq!(screen.filtered().len(), 1);
        let icons = palette_icon_vertices_with(Some(&registry), &screen, &layout, 1280, 720);
        assert_eq!(
            icons.block.len(),
            18,
            "one filtered block must draw one cube icon"
        );
        assert!(
            icons.sprite.is_empty(),
            "a filtered block must not draw sprite icons"
        );

        // One authored item match: exactly one 6-vertex sprite, no cubes.
        screen.apply(TextInputAction::Clear);
        screen.apply(TextInputAction::InsertText("mymod:ruby_pickaxe".into()));
        assert_eq!(screen.filtered().len(), 1);
        let icons = palette_icon_vertices_with(Some(&registry), &screen, &layout, 1280, 720);
        assert!(
            icons.block.is_empty(),
            "a filtered item must not draw block cubes"
        );
        assert_eq!(
            icons.sprite.len(),
            6,
            "one filtered item must draw one sprite icon"
        );
    }

    #[test]
    fn palette_icons_resolve_pack_blocks_and_items_through_the_registry() {
        let registry = Arc::new(reference_registry());
        let mut screen = PaletteScreen::new();
        screen.open(&registry);
        let layout = palette::layout(1280, 720);

        assert!(
            screen
                .entries()
                .iter()
                .any(|entry| entry.name == "mymod:ruby_ore"),
            "the pack block must be listed"
        );
        assert!(
            screen
                .entries()
                .iter()
                .any(|entry| entry.name == "mymod:ruby"),
            "the pack item must be listed"
        );

        let icons = palette_icon_vertices_with(Some(&registry), &screen, &layout, 1280, 720);
        assert!(
            icons
                .block
                .iter()
                .any(|vertex| vertex.layer >= u32::from(PACK_LAYER_BASE)),
            "the pack block icon must sample a registry-authored layer"
        );
        assert!(
            icons
                .sprite
                .iter()
                .any(|vertex| vertex.layer >= u32::from(PACK_ICON_BASE)),
            "the pack item/tool icons must sample registry-authored layers"
        );
    }
}
