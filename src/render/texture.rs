//! Block-face texture identity and the CPU composers behind the GPU block
//! texture array.
//!
//! Everything here is GPU-free: [`generate_all_texture_layers_rgba`] owns the
//! authored/procedural base art and [`compose_block_array_layers`] folds pack
//! layers into the 256-layer upload contract. [`compose_atlas_rgba`] and
//! [`pinned_atlas_layers`] turn that composed data into the deterministic
//! `--demo-atlas` contact sheet. The atlas deliberately stops at the last
//! authored base layer plus the pack slots and **excludes the reserved
//! placeholder band** (`PACK_LAYER_BASE + pack_slot_count ..=
//! BLOCK_ARRAY_LAYERS`), so the artifact pins authored pixels rather than the
//! checker filler that exists only to satisfy the fixed GPU array shape.

use crate::content::{BlockAlpha, PACK_LAYER_BASE, PACK_LAYER_CAP};
use opencraft_gfx::texture::TextureArraySpec;

pub const TEX_SIZE: u32 = 16;
/// Block texture array layers.
///
/// Layers 0..=25 are authored block art (26 layers, see
/// [`crate::pack::LAYER_FILES`]); 26 is built-in socket-marker art that packs
/// cannot override; 27..=31 are reserved block placeholders no pack can
/// override. Layers 32..=44 are the 13 authorable mob part layers
/// (see [`crate::pack::MOB_LAYER_FILES`]) and 45..=47 are reserved spares.
/// The WGSL `w1 & 255` wire format still holds because all 48 layers fit in
/// a byte with room to spare.
pub const NUM_TEX_LAYERS: u32 = 48;
/// Total GPU layers in the block texture array.
///
/// The authored/procedural contract stays [`NUM_TEX_LAYERS`] (48) layers; the
/// GPU array is grown to 256 so pack-authored block faces can live at
/// [`PACK_LAYER_BASE`]..[`PACK_LAYER_CAP`] without changing the wire format
/// (the WGSL `w1 & 255` decode already addresses 0..=255). Every remaining
/// layer is filled with the reserved checker placeholder.
pub const BLOCK_ARRAY_LAYERS: u32 = 256;

const _: () = assert!(PACK_LAYER_BASE as u32 == NUM_TEX_LAYERS);
const _: () = assert!(PACK_LAYER_CAP as u32 <= BLOCK_ARRAY_LAYERS);

/// Bytes per 16x16 RGBA layer.
const LAYER_BYTES: usize = (TEX_SIZE * TEX_SIZE * 4) as usize;
/// First array layer holding mob part art ([`MOB_LAYER_BASE`]..=[`MOB_ARROW_TEX_LAYER`]).
pub const MOB_LAYER_BASE: u32 = 32;
/// Count of mob part layers a texture pack may author.
pub const MOB_AUTHORABLE_TEX_LAYERS: usize = 13;
/// Array layer for the arrow projectile texture (last authorable mob layer).
pub const MOB_ARROW_TEX_LAYER: u32 = 44;
/// First of the three reserved spare layers after the mob range.
pub const MOB_SPARE_LAYER_BASE: u32 = 45;
pub const MIP_LEVELS: u32 = 5;

/// Neutral upload/mip spec for the block texture array. The game keeps every
/// content-identity constant (16px layers, 256 GPU layers, 5 mips);
/// [`opencraft_gfx::texture`] owns the device-facing descriptor, view, and
/// upload machinery that consumes this shape. Public because the host now
/// passes it into the renderer constructor, which creates the texture/view/
/// sampler from it.
pub const BLOCK_ARRAY_SPEC: TextureArraySpec = TextureArraySpec {
    label: "block_texture_array",
    size: TEX_SIZE,
    layers: BLOCK_ARRAY_LAYERS,
    mip_levels: MIP_LEVELS,
};

#[inline]
fn hash_pixel(x: u32, y: u32, seed: u32) -> u32 {
    let mut h = x
        .wrapping_mul(374_761_393)
        .wrapping_add(y.wrapping_mul(668_265_263))
        ^ seed.wrapping_mul(2_246_822_519);
    h = (h ^ (h >> 13)).wrapping_mul(3_266_489_917);
    h ^ (h >> 16)
}

#[inline]
fn clamp_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Alpha-returning layer painter: the closure yields `[r, g, b, a]` per pixel.
fn make_layer_rgba<F: Fn(u32, u32, i32) -> [u8; 4]>(seed: u32, f: F) -> Vec<u8> {
    let mut data = vec![0u8; (TEX_SIZE * TEX_SIZE * 4) as usize];
    for y in 0..TEX_SIZE {
        for x in 0..TEX_SIZE {
            let h = hash_pixel(x, y, seed);
            let jitter = (h & 0xF) as i32 - 8;
            let rgba = f(x, y, jitter);
            let idx = ((y * TEX_SIZE + x) * 4) as usize;
            data[idx..idx + 4].copy_from_slice(&rgba);
        }
    }
    data
}

/// Opaque layer painter: forces alpha to 255 for every pixel.
fn make_layer<F: Fn(u32, u32, i32) -> [u8; 3]>(seed: u32, f: F) -> Vec<u8> {
    make_layer_rgba(seed, move |x, y, jitter| {
        let [r, g, b] = f(x, y, jitter);
        [r, g, b, 255]
    })
}

/// Alpha class a fixed texture-array layer must be rendered with.
///
/// Leaves (layer 8) and the four wheat stages (17..=20) are binary cutout
/// art; glass (layer 25) is a semi-transparent blend layer. Every other built-in
/// and procedural layer — including the socket marker, reserved checkers, mob
/// parts and spares — stays fully opaque, so the blanket "all art is opaque"
/// invariant is replaced by this explicit per-layer policy.
pub fn layer_alpha_policy(index: u32) -> BlockAlpha {
    match index {
        8 | 17..=20 => BlockAlpha::Cutout,
        25 => BlockAlpha::Blend,
        _ => BlockAlpha::Opaque,
    }
}

/// Deterministic wheat art shared by stages 17..=20: vertical stalks that grow
/// taller and ripen from green to gold as `stalk_height` and `ripeness`
/// (0..=3) increase.
///
/// The background is fully transparent (alpha 0) so wheat renders as a staged
/// cross plant and the cutout pass discards the gaps. This intentionally
/// supersedes MEM323's opaque tilled-soil backdrop.
fn wheat_layer_rgba(seed: u32, stalk_height: u32, ripeness: u32) -> Vec<u8> {
    let green = [80i32, 148, 46];
    let gold = [208i32, 172, 56];
    let mix =
        |channel: usize| green[channel] + (gold[channel] - green[channel]) * ripeness as i32 / 3;
    make_layer_rgba(seed, move |x, y, j| {
        let stalk = x % 4 == 1;
        let jitter_top = hash_pixel(x, 0, seed) % 2;
        let top = 15u32.saturating_sub(stalk_height + jitter_top);
        let on_stalk = stalk && y >= top;
        let head = on_stalk && y + 3 > top;
        let rgb = if head {
            [
                clamp_u8(mix(0) + 20 + j / 2),
                clamp_u8(mix(1) + 20 + j / 2),
                clamp_u8(mix(2) + 10 + j / 2),
            ]
        } else if on_stalk {
            [
                clamp_u8(mix(0) + j),
                clamp_u8(mix(1) + j),
                clamp_u8(mix(2) + j),
            ]
        } else {
            // Transparent gap between stalks: the cutout pass never draws it.
            return [0, 0, 0, 0];
        };
        [rgb[0], rgb[1], rgb[2], 255]
    })
}

/// Deterministic glass art for layer 25: a pale blue pane inside a lighter
/// frame with a diagonal glint and a central mullion.
///
/// Every pixel is semi-transparent — alpha is clamped to `1..=254` — so the
/// blend pass composites the pane over the scene instead of discarding it or
/// masking what is behind it entirely.
fn glass_layer_rgba(seed: u32) -> Vec<u8> {
    make_layer_rgba(seed, |x, y, j| {
        let frame = x == 0 || y == 0 || x == 15 || y == 15;
        let mullion = x == 7 || x == 8 || y == 7 || y == 8;
        let glint = (x + y) % 9 == 0 && !frame && !mullion;
        let (r, g, b) = if frame {
            (176 + j / 4, 214 + j / 4, 232 + j / 4)
        } else if mullion {
            (150 + j / 3, 196 + j / 3, 220 + j / 3)
        } else if glint {
            (214 + j / 4, 240 + j / 4, 252 + j / 4)
        } else {
            (120 + j / 2, 176 + j / 2, 212 + j / 2)
        };
        let alpha = if frame {
            230
        } else if mullion {
            176
        } else if glint {
            132
        } else {
            96 + j / 2
        };
        [
            clamp_u8(r),
            clamp_u8(g),
            clamp_u8(b),
            alpha.clamp(1, 254) as u8,
        ]
    })
}

/// Deterministic magenta checker used by every reserved (non-authorable)
/// layer. Opposing shades keep it opaque and visibly distinct from real art.
fn reserved_checker(index: u32) -> Vec<u8> {
    make_layer(100 + index, move |x, y, j| {
        if (x / 2 + y / 2 + index).is_multiple_of(2) {
            [
                clamp_u8(236 + j / 4),
                clamp_u8(32 + j / 4),
                clamp_u8(214 + j / 4),
            ]
        } else {
            [
                clamp_u8(24 + j / 4),
                clamp_u8(8 + j / 4),
                clamp_u8(40 + j / 4),
            ]
        }
    })
}

pub fn generate_all_texture_layers_rgba() -> Vec<Vec<u8>> {
    let mut layers = vec![
        // 0: Grass Top
        make_layer(1, |_, _, j| {
            [clamp_u8(88 + j), clamp_u8(168 + j * 2), clamp_u8(54 + j)]
        }),
        // 1: Grass Side
        make_layer(2, |x, y, j| {
            let fringe = 2 + (hash_pixel(x, 0, 99) % 3);
            if y < fringe {
                [clamp_u8(88 + j), clamp_u8(168 + j * 2), clamp_u8(54 + j)]
            } else {
                [clamp_u8(134 + j * 2), clamp_u8(96 + j), clamp_u8(67 + j)]
            }
        }),
        // 2: Dirt
        make_layer(3, |_, _, j| {
            [clamp_u8(134 + j * 2), clamp_u8(96 + j), clamp_u8(67 + j)]
        }),
        // 3: Stone
        make_layer(4, |x, y, j| {
            let streak = if (x + y * 3) % 7 == 0 { -10 } else { 0 };
            let v = clamp_u8(128 + j + streak);
            [v, v, clamp_u8(v as i32 + 3)]
        }),
        // 4: Cobblestone
        make_layer(5, |x, y, j| {
            let mortar = (x % 4 == 0 && y % 2 == 0) || (y % 4 == 0 && x % 2 == 1);
            let base = if mortar { 82 } else { 122 + j * 2 };
            let v = clamp_u8(base);
            [v, v, v]
        }),
        // 5: Sand
        make_layer(6, |_, y, j| {
            let wave = ((y % 4) as i32 - 2) * 3;
            [
                clamp_u8(224 + j + wave),
                clamp_u8(210 + j + wave),
                clamp_u8(158 + j + wave),
            ]
        }),
        // 6: Oak Log Top
        make_layer(7, |x, y, j| {
            let dx = (x as i32 - 7).abs().max((y as i32 - 7).abs());
            if dx >= 7 {
                [clamp_u8(102 + j), clamp_u8(78 + j), clamp_u8(48 + j)]
            } else {
                let ring = if dx % 2 == 0 { 12 } else { 0 };
                [
                    clamp_u8(168 + ring + j),
                    clamp_u8(136 + ring + j),
                    clamp_u8(86 + ring + j),
                ]
            }
        }),
        // 7: Oak Log Side
        make_layer(8, |x, _, j| {
            let stripe = if x % 3 == 0 { -14 } else { 6 };
            [
                clamp_u8(104 + stripe + j),
                clamp_u8(80 + stripe + j),
                clamp_u8(50 + stripe + j),
            ]
        }),
        // 8: Oak Leaves - the established leaf RGB art with deterministic
        // fully transparent holes so the cutout pass masks the gaps.
        make_layer_rgba(9, |x, y, j| {
            let dark = if (x + y) % 3 == 0 { -16 } else { 8 };
            let hole = hash_pixel(x, y, 4_873).is_multiple_of(4);
            [
                clamp_u8(52 + dark + j),
                clamp_u8(132 + dark + j * 2),
                clamp_u8(38 + dark + j),
                if hole { 0 } else { 255 },
            ]
        }),
        // 9: Oak Planks
        make_layer(10, |x, y, j| {
            let seam = y % 4 == 0 || (x == ((y / 4) * 7) % 16);
            let d = if seam { -26 } else { 0 };
            [
                clamp_u8(172 + d + j),
                clamp_u8(138 + d + j),
                clamp_u8(88 + d + j),
            ]
        }),
        // 10: Brick
        make_layer(11, |x, y, j| {
            let row = y / 4;
            let mortar = y % 4 == 0 || ((x + row * 4) % 8 == 0);
            if mortar {
                [clamp_u8(190 + j), clamp_u8(186 + j), clamp_u8(178 + j)]
            } else {
                [clamp_u8(168 + j * 2), clamp_u8(74 + j), clamp_u8(62 + j)]
            }
        }),
        // 11: Snow Top
        make_layer(12, |_, _, j| {
            [
                clamp_u8(242 + j / 2),
                clamp_u8(248 + j / 2),
                clamp_u8(255 + j / 2),
            ]
        }),
        // 12: Bedrock
        make_layer(13, |_, _, j| {
            let v = clamp_u8(54 + j * 4);
            [v, v, v]
        }),
        // 13: Snow Side
        make_layer(14, |x, y, j| {
            let fringe = 3 + (hash_pixel(x, 0, 123) % 3);
            if y < fringe {
                [
                    clamp_u8(242 + j / 2),
                    clamp_u8(248 + j / 2),
                    clamp_u8(255 + j / 2),
                ]
            } else {
                [clamp_u8(134 + j * 2), clamp_u8(96 + j), clamp_u8(67 + j)]
            }
        }),
        // 14: Coal Ore - stone base with clustered near-black lumps
        make_layer(23, |x, y, j| {
            let lump_a = (x as i32 - 4).pow(2) + (y as i32 - 5).pow(2) < 6;
            let lump_b = (x as i32 - 11).pow(2) + (y as i32 - 10).pow(2) < 8;
            let lump_c = (x as i32 - 10).pow(2) + (y as i32 - 3).pow(2) < 3;
            if lump_a || lump_b || lump_c {
                let v = clamp_u8(26 + j / 2);
                [v, v, clamp_u8(v as i32 + 2)]
            } else {
                let streak = if (x + y * 3) % 7 == 0 { -10 } else { 0 };
                let v = clamp_u8(128 + j + streak);
                [v, v, clamp_u8(v as i32 + 3)]
            }
        }),
        // 15: Iron Ore - stone base with clustered warm tan/rust lumps
        make_layer(24, |x, y, j| {
            let lump_a = (x as i32 - 5).pow(2) + (y as i32 - 10).pow(2) < 7;
            let lump_b = (x as i32 - 11).pow(2) + (y as i32 - 4).pow(2) < 6;
            let lump_c = (x as i32 - 2).pow(2) + (y as i32 - 2).pow(2) < 3;
            if lump_a || lump_b || lump_c {
                let rust = (hash_pixel(x, y, 77) % 24) as i32;
                [
                    clamp_u8(184 + j / 2 - rust / 2),
                    clamp_u8(124 + j / 2 - rust / 3),
                    clamp_u8(70 + j / 2),
                ]
            } else {
                let streak = if (x + y * 3) % 7 == 0 { -10 } else { 0 };
                let v = clamp_u8(128 + j + streak);
                [v, v, clamp_u8(v as i32 + 3)]
            }
        }),
        // 16: Farmland - dirt with dark tilled furrows and raised ridges
        make_layer(25, |x, y, j| {
            let furrow = y % 5 == 2 || (y % 5 == 3 && x % 4 == 0);
            let ridge = if y % 5 == 0 { 14 } else { 0 };
            let d = if furrow { -34 } else { 0 };
            [
                clamp_u8(134 + d + ridge + j),
                clamp_u8(96 + d / 2 + ridge + j),
                clamp_u8(67 + d / 3 + ridge + j),
            ]
        }),
        // 17: Wheat Stage 0 - short green sprouts
        wheat_layer_rgba(26, 4, 0),
        // 18: Wheat Stage 1 - taller green sprouts
        wheat_layer_rgba(27, 7, 1),
        // 19: Wheat Stage 2 - tall, greening gold
        wheat_layer_rgba(28, 10, 2),
        // 20: Wheat Stage 3 - full height, ripe golden wheat
        wheat_layer_rgba(29, 13, 3),
        // 21: Chest - plank body, dark band and latch
        make_layer(30, |x, y, j| {
            let border = x == 0 || y == 0 || x == 15 || y == 15;
            let band = y == 6 || y == 7;
            let latch = (x == 7 || x == 8) && (6..=8).contains(&y);
            if border {
                [
                    clamp_u8(96 + j / 2),
                    clamp_u8(62 + j / 2),
                    clamp_u8(32 + j / 2),
                ]
            } else if latch {
                [
                    clamp_u8(232 + j / 2),
                    clamp_u8(196 + j / 2),
                    clamp_u8(72 + j / 2),
                ]
            } else if band {
                [
                    clamp_u8(78 + j / 2),
                    clamp_u8(52 + j / 2),
                    clamp_u8(28 + j / 2),
                ]
            } else {
                let seam = y % 4 == 0;
                let d = if seam { -22 } else { 0 };
                [
                    clamp_u8(176 + d + j),
                    clamp_u8(132 + d + j),
                    clamp_u8(74 + d + j),
                ]
            }
        }),
        // 22: Loot Chest - darker body with gold trim, distinct from 21
        make_layer(31, |x, y, j| {
            let border = x == 0 || y == 0 || x == 15 || y == 15;
            let trim = x == 2 || x == 13 || y == 2 || y == 13;
            let latch = (x == 7 || x == 8) && (6..=9).contains(&y);
            if latch || (border && (x + y) % 3 == 0) {
                [
                    clamp_u8(238 + j / 3),
                    clamp_u8(206 + j / 3),
                    clamp_u8(88 + j / 3),
                ]
            } else if border {
                [
                    clamp_u8(56 + j / 2),
                    clamp_u8(36 + j / 2),
                    clamp_u8(78 + j / 2),
                ]
            } else if trim {
                [
                    clamp_u8(214 + j / 3),
                    clamp_u8(178 + j / 3),
                    clamp_u8(64 + j / 3),
                ]
            } else {
                let seam = (x / 5) % 2 == 0;
                let d = if seam { 0 } else { -16 };
                [
                    clamp_u8(88 + d + j),
                    clamp_u8(64 + d + j),
                    clamp_u8(112 + d + j),
                ]
            }
        }),
        // 23: Crafting Table Top - warm workbench board with a carved grid,
        // dark border, and small metal/wood tool accents.
        make_layer(32, |x, y, j| {
            let border = x == 0 || y == 0 || x == 15 || y == 15;
            let grid = x % 5 == 0 || y % 5 == 0;
            let handle = x == 12 && (3..=7).contains(&y);
            let blade = (3..=5).contains(&x) && y == 11;
            let nail = (x == 9 && y == 4) || (x == 6 && y == 9);
            if border {
                [
                    clamp_u8(92 + j / 2),
                    clamp_u8(58 + j / 2),
                    clamp_u8(30 + j / 2),
                ]
            } else if blade {
                [
                    clamp_u8(184 + j / 3),
                    clamp_u8(184 + j / 3),
                    clamp_u8(192 + j / 3),
                ]
            } else if handle {
                [
                    clamp_u8(120 + j / 2),
                    clamp_u8(78 + j / 2),
                    clamp_u8(40 + j / 2),
                ]
            } else if nail {
                [
                    clamp_u8(214 + j / 3),
                    clamp_u8(168 + j / 3),
                    clamp_u8(78 + j / 3),
                ]
            } else if grid {
                [
                    clamp_u8(126 + j / 2),
                    clamp_u8(86 + j / 2),
                    clamp_u8(44 + j / 2),
                ]
            } else {
                [clamp_u8(188 + j), clamp_u8(140 + j), clamp_u8(82 + j)]
            }
        }),
        // 24: Crafting Table Side - plank-toned body, darker top band, and
        // vertical legs joined by a horizontal brace.
        make_layer(33, |x, y, j| {
            let top_band = y < 3;
            let leg = x <= 2 || x >= 13;
            let brace = (3..=12).contains(&x) && (9..=10).contains(&y);
            let seam = y % 4 == 0;
            if top_band {
                [
                    clamp_u8(110 + j / 2),
                    clamp_u8(72 + j / 2),
                    clamp_u8(36 + j / 2),
                ]
            } else if brace {
                let d = if seam { -10 } else { 0 };
                [
                    clamp_u8(118 + d + j / 2),
                    clamp_u8(82 + d + j / 2),
                    clamp_u8(44 + d + j / 2),
                ]
            } else if leg {
                [clamp_u8(96 + j), clamp_u8(64 + j), clamp_u8(32 + j)]
            } else {
                let d = if seam { -20 } else { 0 };
                [
                    clamp_u8(160 + d + j),
                    clamp_u8(116 + d + j),
                    clamp_u8(64 + d + j),
                ]
            }
        }),
        // 25: Glass - pale blue pane: a blend layer whose alpha never reaches
        // 0 or 255, so the sorted translucent pass composites it.
        glass_layer_rgba(34),
        // 26: Socket Marker - dark slate frame around a cyan inset arrow, so an
        // authoring socket reads as a scanning point instead of the magenta
        // checker. Built-in art only: no pack file name maps to this layer.
        make_layer(35, |x, y, j| {
            let outer = x < 2 || y < 2 || x > 13 || y > 13;
            let inner = x == 3 || y == 3 || x == 12 || y == 12;
            let head = (y == 5 && (6..=9).contains(&x))
                || (y == 6 && (5..=10).contains(&x))
                || (y == 7 && (4..=11).contains(&x));
            let shaft = (7..=8).contains(&x) && (8..=12).contains(&y);
            if outer {
                [
                    clamp_u8(38 + j / 3),
                    clamp_u8(46 + j / 3),
                    clamp_u8(58 + j / 3),
                ]
            } else if inner {
                [
                    clamp_u8(74 + j / 3),
                    clamp_u8(86 + j / 3),
                    clamp_u8(104 + j / 3),
                ]
            } else if head || shaft {
                [
                    clamp_u8(44 + j / 3),
                    clamp_u8(214 + j / 2),
                    clamp_u8(222 + j / 2),
                ]
            } else {
                let scan = (x + y) % 4 == 0;
                let d = if scan { 10 } else { 0 };
                [
                    clamp_u8(22 + d + j / 2),
                    clamp_u8(30 + d + j / 2),
                    clamp_u8(40 + d + j / 2),
                ]
            }
        }),
    ];

    // 27..32: reserved block layers. They are not authorable (no pack file
    // name maps to them). A deterministic magenta checker, offset per layer,
    // keeps each one opaque and visibly distinct from every authored layer.
    layers.extend((27..MOB_LAYER_BASE).map(reserved_checker));

    // 32..=44: the 13 authorable mob part layers, in `MOB_LAYER_FILES` order.
    // Each is deterministic, opaque (make_layer forces alpha 255), and
    // patterned so it is never a flat color.
    layers.extend([
        // 32: pig_body - pink hide with horizontal banding.
        make_layer(200, |_, y, j| {
            let band = if y % 4 == 0 { -10 } else { 0 };
            [
                clamp_u8(236 + j + band),
                clamp_u8(154 + j / 2 + band),
                clamp_u8(162 + j / 2 + band),
            ]
        }),
        // 33: pig_head - lighter pink with a darker snout block.
        make_layer(201, |x, y, j| {
            let snout = (5..=10).contains(&x) && (6..=11).contains(&y);
            if snout {
                [clamp_u8(212 + j), clamp_u8(118 + j), clamp_u8(126 + j)]
            } else {
                [
                    clamp_u8(244 + j),
                    clamp_u8(164 + j / 2),
                    clamp_u8(170 + j / 2),
                ]
            }
        }),
        // 34: pig_leg - darker pink with vertical seams.
        make_layer(202, |x, _, j| {
            let seam = if x % 4 == 0 { -14 } else { 0 };
            [
                clamp_u8(222 + j + seam),
                clamp_u8(132 + j / 2 + seam),
                clamp_u8(142 + j / 2 + seam),
            ]
        }),
        // 35: cow_body - off-white hide with brown patches.
        make_layer(203, |x, y, j| {
            let patch = (x / 6 + y / 5) % 3 == 0;
            if patch {
                [clamp_u8(112 + j), clamp_u8(76 + j), clamp_u8(52 + j)]
            } else {
                [clamp_u8(238 + j), clamp_u8(234 + j), clamp_u8(226 + j)]
            }
        }),
        // 36: cow_head - white face with a tan muzzle.
        make_layer(204, |x, y, j| {
            let muzzle = (4..=11).contains(&x) && (8..=13).contains(&y);
            if muzzle {
                [clamp_u8(204 + j), clamp_u8(164 + j), clamp_u8(148 + j)]
            } else {
                [clamp_u8(242 + j), clamp_u8(238 + j), clamp_u8(232 + j)]
            }
        }),
        // 37: cow_leg - white with dark hooves at the bottom.
        make_layer(205, |_, y, j| {
            if y >= 13 {
                [
                    clamp_u8(62 + j / 2),
                    clamp_u8(48 + j / 2),
                    clamp_u8(42 + j / 2),
                ]
            } else {
                [clamp_u8(232 + j), clamp_u8(226 + j), clamp_u8(216 + j)]
            }
        }),
        // 38: zombie_body - green skin with torn darker patches.
        make_layer(206, |x, y, j| {
            let torn = (x / 5 + y / 4) % 4 == 0;
            if torn {
                [clamp_u8(54 + j), clamp_u8(94 + j), clamp_u8(46 + j)]
            } else {
                [clamp_u8(84 + j), clamp_u8(136 + j), clamp_u8(72 + j)]
            }
        }),
        // 39: zombie_head - green face with two dark eye sockets.
        make_layer(207, |x, y, j| {
            let eye = (3..=5).contains(&x) || (10..=12).contains(&x);
            if eye && (5..=8).contains(&y) {
                [
                    clamp_u8(24 + j / 2),
                    clamp_u8(34 + j / 2),
                    clamp_u8(22 + j / 2),
                ]
            } else {
                [clamp_u8(100 + j), clamp_u8(154 + j), clamp_u8(84 + j)]
            }
        }),
        // 40: zombie_leg - dark green with a vertical seam.
        make_layer(208, |x, _, j| {
            let seam = if x % 5 == 0 { -18 } else { 0 };
            [
                clamp_u8(68 + j + seam),
                clamp_u8(108 + j + seam),
                clamp_u8(60 + j + seam),
            ]
        }),
        // 41: skeleton_body - bone grey with rib striping.
        make_layer(209, |_, y, j| {
            let rib = if y % 3 == 0 { -22 } else { 0 };
            [
                clamp_u8(198 + j + rib),
                clamp_u8(198 + j + rib),
                clamp_u8(190 + j + rib),
            ]
        }),
        // 42: skeleton_head - bone grey with two dark eye sockets.
        make_layer(210, |x, y, j| {
            let eye = (3..=5).contains(&x) || (10..=12).contains(&x);
            if eye && (6..=8).contains(&y) {
                [
                    clamp_u8(48 + j / 2),
                    clamp_u8(48 + j / 2),
                    clamp_u8(44 + j / 2),
                ]
            } else {
                [clamp_u8(206 + j), clamp_u8(206 + j), clamp_u8(198 + j)]
            }
        }),
        // 43: skeleton_leg - bone grey with joint segments.
        make_layer(211, |_, y, j| {
            let joint = if y % 5 == 4 { -20 } else { 0 };
            [
                clamp_u8(190 + j + joint),
                clamp_u8(190 + j + joint),
                clamp_u8(182 + j + joint),
            ]
        }),
        // 44: arrow - wooden shaft, grey head, pale fletching.
        make_layer(212, |x, y, j| {
            if y <= 3 {
                [clamp_u8(150 + j), clamp_u8(150 + j), clamp_u8(146 + j)]
            } else if y >= 12 {
                [clamp_u8(226 + j), clamp_u8(222 + j), clamp_u8(206 + j)]
            } else if (7..=8).contains(&x) {
                [clamp_u8(124 + j), clamp_u8(82 + j), clamp_u8(48 + j)]
            } else {
                [clamp_u8(96 + j), clamp_u8(64 + j), clamp_u8(38 + j)]
            }
        }),
    ]);

    // 45..48: reserved spare layers, same magenta checker rule as 27..32.
    layers.extend((MOB_SPARE_LAYER_BASE..NUM_TEX_LAYERS).map(reserved_checker));

    debug_assert_eq!(layers.len(), NUM_TEX_LAYERS as usize);
    debug_assert_eq!(
        MOB_LAYER_BASE as usize + MOB_AUTHORABLE_TEX_LAYERS,
        MOB_ARROW_TEX_LAYER as usize + 1
    );
    layers
}

/// Halve a square RGBA image with a 2x box average (edge-clamped).
/// Shared by the texture-array mip chain and the pack resample path so both
/// use one averaging rule. The implementation lives in `opencraft-core`; this
/// re-export keeps every `crate::render::texture::downsample_rgba_2x` caller
/// and the source-pin mip-rule test on one path.
pub use opencraft_core::rgba::downsample_rgba_2x;

/// True when `layers` matches the fixed block texture array contract:
/// exactly [`NUM_TEX_LAYERS`] layers, each [`TEX_SIZE`] × [`TEX_SIZE`] × 4
/// RGBA bytes. Used to guard live re-uploads (MEM111) in debug builds.
pub fn block_layers_match_shape(layers: &[Vec<u8>]) -> bool {
    layers.len() == NUM_TEX_LAYERS as usize && layers.iter().all(|layer| layer.len() == LAYER_BYTES)
}

/// True when `layers` matches the fixed 256-layer GPU block array contract:
/// exactly [`BLOCK_ARRAY_LAYERS`] layers, each [`TEX_SIZE`] × [`TEX_SIZE`] × 4
/// RGBA bytes. Used to guard live re-uploads (MEM111) in debug builds.
pub fn block_array_layers_match_shape(layers: &[Vec<u8>]) -> bool {
    BLOCK_ARRAY_SPEC.matches_shape(layers)
}

/// Compose the 256-layer GPU block array from the 48 authored/procedural base
/// layers plus the frozen session registry's pack layers.
///
/// Layer indices 0..=47 are copied byte-for-byte from `base` (the built-in
/// presentation contract), pack layers fill 48..=207 in registry order, and
/// 208..=255 are reserved checker placeholders. `base` is never mutated. A
/// malformed base or pack layer is replaced by its reserved checker rather
/// than panicking so a bad pack can never take down the renderer; the output
/// shape is always 256 × 1024 bytes.
pub fn compose_block_array_layers(base: &[Vec<u8>], pack_layers: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut layers = Vec::with_capacity(BLOCK_ARRAY_LAYERS as usize);

    for index in 0..NUM_TEX_LAYERS as usize {
        match base.get(index) {
            Some(layer) if layer.len() == LAYER_BYTES => layers.push(layer.clone()),
            _ => layers.push(reserved_checker(index as u32)),
        }
    }

    let pack_slot_count = (PACK_LAYER_CAP - PACK_LAYER_BASE) as usize;
    for (slot, layer) in pack_layers.iter().take(pack_slot_count).enumerate() {
        let gpu_index = PACK_LAYER_BASE as usize + slot;
        if layer.len() == LAYER_BYTES {
            layers.push(layer.clone());
        } else {
            layers.push(reserved_checker(gpu_index as u32));
        }
    }

    while layers.len() < BLOCK_ARRAY_LAYERS as usize {
        layers.push(reserved_checker(layers.len() as u32));
    }

    debug_assert!(block_array_layers_match_shape(&layers));
    layers
}

/// Tiles per row in the `--demo-atlas` contact sheet.
pub const ATLAS_COLUMNS: usize = 8;

/// One deterministic, GPU-free contact sheet of 16x16 RGBA tiles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtlasSheet {
    /// Sheet width in pixels (`columns * TEX_SIZE`).
    pub width: u32,
    /// Sheet height in pixels (`ceil(tiles / columns) * TEX_SIZE`).
    pub height: u32,
    /// Row-major RGBA8 pixels, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
    /// Number of 16x16 tiles copied into the sheet.
    pub tiles: usize,
}

/// Tile `layers` row-major into a `columns`-wide grid of 16x16 RGBA tiles.
///
/// Pure integer arithmetic over caller-supplied bytes: no GPU state, no
/// floating point, no `HashMap` iteration and no padding, so the same layer
/// sequence always yields byte-identical output. Every layer must be exactly
/// [`TEX_SIZE`] × [`TEX_SIZE`] × 4 RGBA bytes and `columns` must be non-zero;
/// a malformed input is a caller bug and panics instead of silently shifting
/// the grid. An empty layer set produces a zero-sized sheet.
pub fn compose_atlas_rgba(layers: &[Vec<u8>], columns: usize) -> AtlasSheet {
    assert!(columns > 0, "compose_atlas_rgba needs at least one column");
    for (index, layer) in layers.iter().enumerate() {
        assert_eq!(
            layer.len(),
            LAYER_BYTES,
            "compose_atlas_rgba layer {index} must be {LAYER_BYTES} RGBA bytes, got {}",
            layer.len()
        );
    }

    let rows = layers.len().div_ceil(columns);
    let width = (columns as u32) * TEX_SIZE;
    let height = (rows as u32) * TEX_SIZE;
    let mut rgba = vec![0u8; (width as usize) * (height as usize) * 4];
    let row_bytes = (TEX_SIZE * 4) as usize;
    for (index, layer) in layers.iter().enumerate() {
        let col = index % columns;
        let row = index / columns;
        for y in 0..TEX_SIZE {
            let src = ((y * TEX_SIZE) * 4) as usize;
            let dst = (((row as u32 * TEX_SIZE + y) * width + col as u32 * TEX_SIZE) * 4) as usize;
            rgba[dst..dst + row_bytes].copy_from_slice(&layer[src..src + row_bytes]);
        }
    }

    AtlasSheet {
        width,
        height,
        rgba,
        tiles: layers.len(),
    }
}

/// The exact slice of the composed block array the atlas artifact pins.
///
/// It is the [`NUM_TEX_LAYERS`] authored/procedural base layers (the built-in
/// `0..=47` contract) followed by `pack_layers` in registry order, truncated
/// to the [`PACK_LAYER_BASE`]..[`PACK_LAYER_CAP`] pack budget. The reserved
/// placeholder band above the last real pack slot is deliberately excluded:
/// it is fixed checker filler with no authored identity, so including it would
/// only make the pinned sheet larger, not stronger. `base` is never mutated
/// and a malformed base/pack layer normalises through
/// [`compose_block_array_layers`] exactly like the live renderer upload.
pub fn pinned_atlas_layers(base: &[Vec<u8>], pack_layers: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let pack_slot_count = pack_layers
        .len()
        .min((PACK_LAYER_CAP - PACK_LAYER_BASE) as usize);
    compose_block_array_layers(base, pack_layers)
        .into_iter()
        .take(NUM_TEX_LAYERS as usize + pack_slot_count)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_procedural_texture_layers_non_empty() {
        let layers = generate_all_texture_layers_rgba();
        assert_eq!(layers.len(), NUM_TEX_LAYERS as usize);
        assert_eq!(layers.len(), 48);
        for (index, layer) in layers.iter().enumerate() {
            assert_eq!(layer.len(), (TEX_SIZE * TEX_SIZE * 4) as usize);
            let (pixels, remainder) = layer.as_chunks::<4>();
            assert!(remainder.is_empty());
            let alpha: Vec<u8> = pixels.iter().map(|px| px[3]).collect();
            match layer_alpha_policy(index as u32) {
                BlockAlpha::Opaque => assert!(
                    alpha.iter().all(|a| *a == 255),
                    "layer {index} must be fully opaque"
                ),
                BlockAlpha::Cutout => {
                    assert!(
                        alpha.iter().any(|a| *a < 128),
                        "cutout layer {index} needs transparent pixels"
                    );
                    assert!(
                        alpha.iter().any(|a| *a >= 128),
                        "cutout layer {index} needs opaque pixels"
                    );
                }
                BlockAlpha::Blend => assert!(
                    alpha.iter().all(|a| (1..=254).contains(a)),
                    "blend layer {index} alpha must stay in 1..=254"
                ),
            }
        }
    }

    #[test]
    fn test_layer_alpha_policy_names_the_cutout_and_blend_layers() {
        assert_eq!(layer_alpha_policy(8), BlockAlpha::Cutout);
        for index in 17..=20 {
            assert_eq!(
                layer_alpha_policy(index),
                BlockAlpha::Cutout,
                "wheat {index}"
            );
        }
        assert_eq!(layer_alpha_policy(25), BlockAlpha::Blend, "glass");
        // Socket marker, reserved ranges, mob layers and spares stay opaque.
        for index in [0u32, 7, 9, 16, 21, 24, 26, 27, 31, 32, 44, 45, 47] {
            assert_eq!(
                layer_alpha_policy(index),
                BlockAlpha::Opaque,
                "layer {index}"
            );
        }
    }

    #[test]
    fn test_block_layer_shape_accepts_canonical_layers() {
        assert!(block_layers_match_shape(&generate_all_texture_layers_rgba()));
    }

    #[test]
    fn test_block_layer_shape_rejects_wrong_layer_count() {
        let mut too_few = generate_all_texture_layers_rgba();
        too_few.pop();
        assert!(!block_layers_match_shape(&too_few));

        let mut too_many = generate_all_texture_layers_rgba();
        too_many.push(vec![0u8; (TEX_SIZE * TEX_SIZE * 4) as usize]);
        assert!(!block_layers_match_shape(&too_many));

        assert!(!block_layers_match_shape(&[]));
    }

    #[test]
    fn test_block_layer_shape_rejects_wrong_layer_size() {
        let mut short = generate_all_texture_layers_rgba();
        short[0].pop();
        assert!(!block_layers_match_shape(&short));

        let mut long = generate_all_texture_layers_rgba();
        long[0].push(0);
        assert!(!block_layers_match_shape(&long));
    }

    #[test]
    fn test_new_block_texture_layers_are_distinct() {
        let layers = generate_all_texture_layers_rgba();
        let authored_new = [16usize, 17, 18, 19, 20, 21, 22, 23, 24, 25];

        // Every new layer differs from every other new layer.
        for (offset, a) in authored_new.iter().enumerate() {
            for b in &authored_new[offset + 1..] {
                assert_ne!(layers[*a], layers[*b], "layers {a} and {b} must differ");
            }
        }

        // ... and from the nearest existing art.
        assert_ne!(layers[16], layers[2], "farmland must differ from dirt");
        for index in [17usize, 18, 19, 20] {
            assert_ne!(layers[index], layers[0], "wheat {index} vs grass top");
            assert_ne!(layers[index], layers[8], "wheat {index} vs leaves");
        }
        assert_ne!(layers[21], layers[9], "chest must differ from planks");
        assert_ne!(layers[22], layers[21], "loot chest must differ from chest");
        // The crafting-table faces ship dedicated art instead of reusing the
        // oak-plank layer (9) that the block sampled before this change.
        assert_ne!(layers[23], layers[9], "table top must differ from planks");
        assert_ne!(layers[24], layers[9], "table side must differ from planks");
        assert_ne!(layers[23], layers[24], "table top and side must differ");
        // Layer 26 is built-in socket-marker art, not reserved checker art.
        for existing in 0..=25usize {
            assert_ne!(
                layers[26], layers[existing],
                "socket marker must differ from built-in layer {existing}"
            );
        }
        assert_ne!(layers[26], layers[23], "socket marker vs table top");
        assert_ne!(layers[26], layers[24], "socket marker vs table side");
        assert_ne!(
            layers[26],
            reserved_checker(26),
            "socket marker must not be the reserved checker"
        );

        // Every new layer is real art, not a flat color, and its alpha bytes
        // must match the declared per-layer class.
        for index in authored_new {
            let (pixels, _) = layers[index].as_chunks::<4>();
            let first = &pixels[0];
            assert!(
                pixels.iter().any(|px| px != first),
                "layer {index} must not be a flat color"
            );
            let alpha: Vec<u8> = pixels.iter().map(|px| px[3]).collect();
            match layer_alpha_policy(index as u32) {
                BlockAlpha::Opaque => assert!(
                    alpha.iter().all(|a| *a == 255),
                    "layer {index} must stay opaque"
                ),
                BlockAlpha::Cutout => {
                    assert!(
                        alpha.iter().any(|a| *a < 128),
                        "cutout layer {index} needs transparent pixels"
                    );
                    assert!(
                        alpha.iter().any(|a| *a >= 128),
                        "cutout layer {index} needs opaque pixels"
                    );
                }
                BlockAlpha::Blend => assert!(
                    alpha.iter().all(|a| (1..=254).contains(a)),
                    "blend layer {index} alpha must stay in 1..=254"
                ),
            }
        }
    }

    #[test]
    fn test_reserved_texture_layers_are_distinct_opaque_placeholders() {
        let layers = generate_all_texture_layers_rgba();
        let builtin = 27usize;
        assert_eq!(
            builtin, 27,
            "built-in procedural coverage is 27 layers (26 authored + marker)"
        );
        assert_eq!(NUM_TEX_LAYERS as usize, 48);

        // Block-reserved 27..32 and spare 45..48 both use the reserved rule;
        // neither range may alias built-in art (including glass 25 and marker
        // 26) or mob art.
        let reserved: Vec<usize> = (builtin..MOB_LAYER_BASE as usize)
            .chain(MOB_SPARE_LAYER_BASE as usize..NUM_TEX_LAYERS as usize)
            .collect();
        assert_eq!(reserved.len(), 5 + 3);

        for &index in &reserved {
            for authored in 0..builtin {
                assert_ne!(
                    layers[index], layers[authored],
                    "reserved layer {index} must differ from authored layer {authored}"
                );
            }
            for mob in (MOB_LAYER_BASE..=MOB_ARROW_TEX_LAYER).map(|i| i as usize) {
                assert_ne!(
                    layers[index], layers[mob],
                    "reserved layer {index} must differ from mob layer {mob}"
                );
            }
            let (pixels, remainder) = layers[index].as_chunks::<4>();
            assert!(remainder.is_empty());
            assert!(
                pixels.iter().all(|px| px[3] == 255),
                "reserved layer {index} must stay opaque"
            );
            let first = &pixels[0];
            assert!(
                pixels.iter().any(|px| px != first),
                "reserved layer {index} must not be a flat color"
            );
        }

        for (offset, a) in reserved.iter().enumerate() {
            for b in &reserved[offset + 1..] {
                assert_ne!(
                    layers[*a], layers[*b],
                    "reserved layers {a} and {b} must differ"
                );
            }
        }
    }

    #[test]
    fn test_mob_texture_layers_are_distinct_opaque_and_indexed() {
        let layers = generate_all_texture_layers_rgba();
        assert_eq!(MOB_LAYER_BASE, 32);
        assert_eq!(MOB_AUTHORABLE_TEX_LAYERS, 13);
        assert_eq!(MOB_ARROW_TEX_LAYER, 44);
        assert_eq!(MOB_SPARE_LAYER_BASE, 45);
        assert_eq!(
            MOB_LAYER_BASE as usize + MOB_AUTHORABLE_TEX_LAYERS,
            MOB_ARROW_TEX_LAYER as usize + 1
        );

        let mob: Vec<usize> = (MOB_LAYER_BASE..=MOB_ARROW_TEX_LAYER)
            .map(|i| i as usize)
            .collect();
        assert_eq!(mob.len(), MOB_AUTHORABLE_TEX_LAYERS);

        for &index in &mob {
            let (pixels, remainder) = layers[index].as_chunks::<4>();
            assert!(remainder.is_empty(), "mob layer {index} shape");
            assert!(
                pixels.iter().all(|px| px[3] == 255),
                "mob layer {index} must be opaque"
            );
            let first = &pixels[0];
            assert!(
                pixels.iter().any(|px| px != first),
                "mob layer {index} must not be a flat color"
            );
            for authored in 0..27usize {
                assert_ne!(
                    layers[index], layers[authored],
                    "mob layer {index} must differ from authored layer {authored}"
                );
            }
        }

        for (offset, a) in mob.iter().enumerate() {
            for b in &mob[offset + 1..] {
                assert_ne!(layers[*a], layers[*b], "mob layers {a} and {b} must differ");
            }
        }
    }

    #[test]
    fn test_block_layer_shape_rejects_num_tex_layers_plus_or_minus_one() {
        let mut fewer = generate_all_texture_layers_rgba();
        fewer.pop();
        assert_eq!(fewer.len(), NUM_TEX_LAYERS as usize - 1);
        assert!(!block_layers_match_shape(&fewer));

        let mut more = generate_all_texture_layers_rgba();
        more.push(vec![0u8; (TEX_SIZE * TEX_SIZE * 4) as usize]);
        assert_eq!(more.len(), NUM_TEX_LAYERS as usize + 1);
        assert!(!block_layers_match_shape(&more));
    }

    #[test]
    fn compose_keeps_base_layers_byte_identical_and_pads_with_checker() {
        let base = generate_all_texture_layers_rgba();
        let composed = compose_block_array_layers(&base, &[]);

        assert_eq!(NUM_TEX_LAYERS, 48);
        assert_eq!(composed.len(), BLOCK_ARRAY_LAYERS as usize);
        assert_eq!(&composed[..NUM_TEX_LAYERS as usize], &base[..]);

        for (index, layer) in composed
            .iter()
            .enumerate()
            .take(BLOCK_ARRAY_LAYERS as usize)
            .skip(NUM_TEX_LAYERS as usize)
        {
            assert_eq!(
                *layer,
                reserved_checker(index as u32),
                "unused layer {index} must be the reserved checker"
            );
        }
        assert!(block_array_layers_match_shape(&composed));
    }

    #[test]
    fn compose_places_pack_layers_at_the_pack_base_and_fills_the_rest() {
        let base = generate_all_texture_layers_rgba();
        let pack0 = vec![0x11u8; LAYER_BYTES];
        let pack1 = vec![0x22u8; LAYER_BYTES];
        let composed = compose_block_array_layers(&base, &[pack0.clone(), pack1.clone()]);

        assert_eq!(composed[PACK_LAYER_BASE as usize], pack0);
        assert_eq!(composed[PACK_LAYER_BASE as usize + 1], pack1);
        for (index, layer) in composed
            .iter()
            .enumerate()
            .take(BLOCK_ARRAY_LAYERS as usize)
            .skip(PACK_LAYER_BASE as usize + 2)
        {
            assert_eq!(*layer, reserved_checker(index as u32));
        }
    }

    #[test]
    fn compose_always_returns_a_256_layer_array_and_rejects_the_48_layer_base() {
        let base = generate_all_texture_layers_rgba();
        let composed = compose_block_array_layers(&base, &[]);

        assert_eq!(composed.len(), 256);
        assert!(composed.iter().all(|layer| layer.len() == 1024));
        assert!(block_array_layers_match_shape(&composed));
        assert!(
            !block_array_layers_match_shape(&base),
            "the 48-layer authored base is not the GPU array shape"
        );
        assert!(!block_array_layers_match_shape(&[]));
    }

    #[test]
    fn compose_caps_pack_layers_at_the_pack_layer_cap() {
        let base = generate_all_texture_layers_rgba();
        let pack: Vec<Vec<u8>> = (0..200u32)
            .map(|index| vec![index as u8; LAYER_BYTES])
            .collect();
        let composed = compose_block_array_layers(&base, &pack);

        for (index, layer) in composed
            .iter()
            .enumerate()
            .take(PACK_LAYER_CAP as usize)
            .skip(PACK_LAYER_BASE as usize)
        {
            assert_eq!(*layer, pack[index - PACK_LAYER_BASE as usize]);
        }
        for (index, layer) in composed
            .iter()
            .enumerate()
            .take(BLOCK_ARRAY_LAYERS as usize)
            .skip(PACK_LAYER_CAP as usize)
        {
            assert_eq!(*layer, reserved_checker(index as u32));
        }
    }

    #[test]
    fn compose_falls_back_to_checker_for_malformed_layers_without_panicking() {
        let base = generate_all_texture_layers_rgba();
        let malformed_pack = vec![9u8; LAYER_BYTES - 1];
        let composed = compose_block_array_layers(&base, &[malformed_pack]);

        assert_eq!(composed[PACK_LAYER_BASE as usize], reserved_checker(48));
        assert!(block_array_layers_match_shape(&composed));

        // A short base is also patched per-index rather than panicking.
        let short_base = base[..NUM_TEX_LAYERS as usize - 1].to_vec();
        let composed = compose_block_array_layers(&short_base, &[]);
        assert_eq!(
            composed[NUM_TEX_LAYERS as usize - 1],
            reserved_checker(NUM_TEX_LAYERS - 1)
        );
        assert!(block_array_layers_match_shape(&composed));
    }

    #[test]
    fn compose_never_mutates_the_base() {
        let base = generate_all_texture_layers_rgba();
        let original = base.clone();
        let _ = compose_block_array_layers(&base, &[vec![7u8; LAYER_BYTES]]);

        assert_eq!(base, original);
        assert_eq!(base.len(), NUM_TEX_LAYERS as usize);
    }

    #[test]
    fn block_array_spec_matches_the_game_contract() {
        assert!(BLOCK_ARRAY_SPEC.validate().is_ok());
        assert_eq!(BLOCK_ARRAY_SPEC.size, TEX_SIZE);
        assert_eq!(BLOCK_ARRAY_SPEC.layers, BLOCK_ARRAY_LAYERS);
        assert_eq!(BLOCK_ARRAY_SPEC.mip_levels, MIP_LEVELS);
        assert_eq!(BLOCK_ARRAY_SPEC.label, "block_texture_array");
        assert_eq!(BLOCK_ARRAY_SPEC.layer_bytes(), LAYER_BYTES);
    }

    #[test]
    fn block_array_shape_rejects_boundary_off_by_one_sizes() {
        let composed = compose_block_array_layers(&generate_all_texture_layers_rgba(), &[]);
        assert!(block_array_layers_match_shape(&composed));

        let mut one_short = composed.clone();
        one_short.pop();
        assert!(!block_array_layers_match_shape(&one_short));

        let mut one_long = composed;
        one_long.push(vec![0u8; LAYER_BYTES]);
        assert!(!block_array_layers_match_shape(&one_long));

        let mut short_layer = generate_all_texture_layers_rgba();
        short_layer[0].pop();
        assert!(!block_array_layers_match_shape(&short_layer));
    }

    #[test]
    fn test_ore_texture_layers_are_distinct_and_non_uniform() {
        let layers = generate_all_texture_layers_rgba();
        let stone = &layers[3];
        let coal = &layers[14];
        let iron = &layers[15];

        assert_ne!(coal, stone, "coal ore must differ from stone");
        assert_ne!(iron, stone, "iron ore must differ from stone");
        assert_ne!(coal, iron, "coal and iron ore must differ");

        let (coal_pixels, _) = coal.as_chunks::<4>();
        let first = &coal_pixels[0];
        assert!(
            coal_pixels.iter().any(|px| px != first),
            "coal ore texture must not be a flat color"
        );
        let (iron_pixels, _) = iron.as_chunks::<4>();
        let first = &iron_pixels[0];
        assert!(
            iron_pixels.iter().any(|px| px != first),
            "iron ore texture must not be a flat color"
        );
    }

    #[test]
    fn atlas_tiles_layers_row_major_without_padding() {
        let layers: Vec<Vec<u8>> = (0..10u8).map(|fill| vec![fill; LAYER_BYTES]).collect();
        let sheet = compose_atlas_rgba(&layers, ATLAS_COLUMNS);

        assert_eq!(sheet.tiles, 10);
        assert_eq!(sheet.width, 8 * TEX_SIZE);
        assert_eq!(sheet.height, 2 * TEX_SIZE);
        assert_eq!(sheet.rgba.len(), (sheet.width * sheet.height * 4) as usize);

        // Tile 0 sits at the top-left and tile 8 starts the second row.
        assert!(sheet.rgba[..4].iter().all(|byte| *byte == 0));
        let tile8 = &sheet.rgba[(sheet.width as usize * TEX_SIZE as usize * 4)..][..4];
        assert!(tile8.iter().all(|byte| *byte == 8));
        // The two unused cells on the second row stay transparent black.
        let tail = &sheet.rgba[sheet.rgba.len() - 4..];
        assert!(tail.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn atlas_for_the_builtin_base_pins_shape_and_digest() {
        // Measured from a real `--demo-atlas <fresh-dir>` run. A change here
        // means the composed block-face pixels drifted; re-pin only together
        // with a deliberate art change.
        const PINNED_RGBA_DIGEST: u64 = 0xaa21_2bc3_f53c_e34c;
        const PINNED_PNG_DIGEST: u64 = 0x4a89_e7b5_ab33_7a8a;
        const PINNED_PNG_BYTES: usize = 32_465;

        let base = generate_all_texture_layers_rgba();
        let layers = pinned_atlas_layers(&base, &[]);
        assert_eq!(layers.len(), NUM_TEX_LAYERS as usize);

        let sheet = compose_atlas_rgba(&layers, ATLAS_COLUMNS);
        assert_eq!(sheet.width, 128);
        assert_eq!(sheet.height, 96);
        assert_eq!(sheet.tiles, 48);
        assert_eq!(sheet.rgba.len(), 49_152);
        assert_eq!(
            crate::pack::defaults::atlas_digest(&sheet.rgba),
            PINNED_RGBA_DIGEST,
            "composed atlas pixels drifted from the pinned digest"
        );

        let png =
            crate::pack::defaults::encode_rgba_png_dims(sheet.width, sheet.height, &sheet.rgba);
        assert_eq!(png.len(), PINNED_PNG_BYTES);
        assert_eq!(
            crate::pack::defaults::atlas_png_digest(&png),
            PINNED_PNG_DIGEST,
            "atlas.png bytes drifted from the pinned digest"
        );
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "atlas must be a PNG");
    }

    #[test]
    fn pinned_atlas_layers_excludes_the_placeholder_band() {
        let base = generate_all_texture_layers_rgba();
        let pack: Vec<Vec<u8>> = (0..3u32)
            .map(|index| vec![0x40 + index as u8; LAYER_BYTES])
            .collect();
        let layers = pinned_atlas_layers(&base, &pack);

        assert_eq!(layers.len(), NUM_TEX_LAYERS as usize + pack.len());
        assert_eq!(&layers[..NUM_TEX_LAYERS as usize], &base[..]);
        assert_eq!(&layers[NUM_TEX_LAYERS as usize..], pack.as_slice());
    }

    #[test]
    #[should_panic(expected = "compose_atlas_rgba layer 0 must be")]
    fn compose_atlas_rgba_rejects_a_malformed_layer() {
        let _ = compose_atlas_rgba(&[vec![0u8; LAYER_BYTES - 1]], ATLAS_COLUMNS);
    }

    #[test]
    #[should_panic(expected = "at least one column")]
    fn compose_atlas_rgba_rejects_zero_columns() {
        let _ = compose_atlas_rgba(&[], 0);
    }
}
