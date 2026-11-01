pub const TEX_SIZE: u32 = 16;
/// Block texture array layers.
///
/// Layers 0..=22 are authored block art (23 layers, see
/// [`crate::pack::LAYER_FILES`]); 23..=31 are reserved block placeholders no
/// pack can override. Layers 32..=44 are the 13 authorable mob part layers
/// (see [`crate::pack::MOB_LAYER_FILES`]) and 45..=47 are reserved spares.
/// The WGSL `w1 & 255` wire format still holds because all 48 layers fit in
/// a byte with room to spare.
pub const NUM_TEX_LAYERS: u32 = 48;
/// First array layer holding mob part art ([`MOB_LAYER_BASE`]..=[`MOB_ARROW_TEX_LAYER`]).
pub const MOB_LAYER_BASE: u32 = 32;
/// Count of mob part layers a texture pack may author.
pub const MOB_AUTHORABLE_TEX_LAYERS: usize = 13;
/// Array layer for the arrow projectile texture (last authorable mob layer).
pub const MOB_ARROW_TEX_LAYER: u32 = 44;
/// First of the three reserved spare layers after the mob range.
pub const MOB_SPARE_LAYER_BASE: u32 = 45;
pub const MIP_LEVELS: u32 = 5;

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

fn make_layer<F: Fn(u32, u32, i32) -> [u8; 3]>(seed: u32, f: F) -> Vec<u8> {
    let mut data = vec![0u8; (TEX_SIZE * TEX_SIZE * 4) as usize];
    for y in 0..TEX_SIZE {
        for x in 0..TEX_SIZE {
            let h = hash_pixel(x, y, seed);
            let jitter = (h & 0xF) as i32 - 8;
            let [r, g, b] = f(x, y, jitter);
            let idx = ((y * TEX_SIZE + x) * 4) as usize;
            data[idx] = r;
            data[idx + 1] = g;
            data[idx + 2] = b;
            data[idx + 3] = 255;
        }
    }
    data
}

/// Deterministic wheat art shared by stages 17..=20: a soil-toned backdrop
/// with vertical stalks that grow taller and ripen from green to gold as
/// `stalk_height` and `ripeness` (0..=3) increase.
fn wheat_layer_rgba(seed: u32, stalk_height: u32, ripeness: u32) -> Vec<u8> {
    let green = [80i32, 148, 46];
    let gold = [208i32, 172, 56];
    let mix =
        |channel: usize| green[channel] + (gold[channel] - green[channel]) * ripeness as i32 / 3;
    make_layer(seed, move |x, y, j| {
        let stalk = x % 4 == 1;
        let jitter_top = hash_pixel(x, 0, seed) % 2;
        let top = 15u32.saturating_sub(stalk_height + jitter_top);
        let on_stalk = stalk && y >= top;
        let head = on_stalk && y + 3 > top;
        if head {
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
            // Tilled soil backdrop so every wheat layer stays fully opaque.
            let d = if (x / 4 + y / 4) % 2 == 0 { 0 } else { -7 };
            [
                clamp_u8(92 + d + j),
                clamp_u8(68 + d + j),
                clamp_u8(46 + d + j),
            ]
        }
    })
}

/// Deterministic magenta checker used by every reserved (non-authorable)
/// layer. Opposing shades keep it opaque and visibly distinct from real art.
fn reserved_checker(index: u32) -> Vec<u8> {
    make_layer(100 + index, move |x, y, j| {
        if (x / 2 + y / 2 + index) % 2 == 0 {
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
        // 8: Oak Leaves
        make_layer(9, |x, y, j| {
            let dark = if (x + y) % 3 == 0 { -16 } else { 8 };
            [
                clamp_u8(52 + dark + j),
                clamp_u8(132 + dark + j * 2),
                clamp_u8(38 + dark + j),
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
    ];

    // 23..32: reserved block layers. They are not authorable (no pack file
    // name maps to them). A deterministic magenta checker, offset per layer,
    // keeps each one opaque and visibly distinct from every authored layer.
    layers.extend((23..MOB_LAYER_BASE).map(|index| reserved_checker(index)));

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

    // 45..48: reserved spare layers, same magenta checker rule as 23..32.
    layers.extend((MOB_SPARE_LAYER_BASE..NUM_TEX_LAYERS).map(|index| reserved_checker(index)));

    debug_assert_eq!(layers.len(), NUM_TEX_LAYERS as usize);
    debug_assert_eq!(
        MOB_LAYER_BASE as usize + MOB_AUTHORABLE_TEX_LAYERS,
        MOB_ARROW_TEX_LAYER as usize + 1
    );
    layers
}

/// Halve a square RGBA image with a 2x box average (edge-clamped).
/// Shared by the texture-array mip chain and the pack resample path so both
/// use one averaging rule.
pub fn downsample_rgba_2x(src: &[u8], src_dim: u32) -> Vec<u8> {
    let dst_dim = (src_dim / 2).max(1);
    let mut dst = vec![0u8; (dst_dim * dst_dim * 4) as usize];
    for y in 0..dst_dim {
        for x in 0..dst_dim {
            let mut sum = [0u32; 4];
            for dy in 0..2 {
                for dx in 0..2 {
                    let sx = (x * 2 + dx).min(src_dim - 1);
                    let sy = (y * 2 + dy).min(src_dim - 1);
                    let sidx = ((sy * src_dim + sx) * 4) as usize;
                    for c in 0..4 {
                        sum[c] += src[sidx + c] as u32;
                    }
                }
            }
            let didx = ((y * dst_dim + x) * 4) as usize;
            for c in 0..4 {
                dst[didx + c] = (sum[c] / 4) as u8;
            }
        }
    }
    dst
}

/// True when `layers` matches the fixed block texture array contract:
/// exactly [`NUM_TEX_LAYERS`] layers, each [`TEX_SIZE`] × [`TEX_SIZE`] × 4
/// RGBA bytes. Used to guard live re-uploads (MEM111) in debug builds.
pub fn block_layers_match_shape(layers: &[Vec<u8>]) -> bool {
    layers.len() == NUM_TEX_LAYERS as usize
        && layers
            .iter()
            .all(|layer| layer.len() == (TEX_SIZE * TEX_SIZE * 4) as usize)
}

/// Upload every layer's base mip and its box-filtered mip chain into an
/// existing texture array.
///
/// Shared by the initial creation path and live pack re-uploads so both use
/// exactly one upload rule (MEM111: the texture object itself is never
/// recreated; only its contents change).
pub fn upload_block_layers(texture: &wgpu::Texture, queue: &wgpu::Queue, layers: &[Vec<u8>]) {
    for (layer_idx, base_rgba) in layers.iter().enumerate() {
        let mut current = base_rgba.clone();
        let mut dim = TEX_SIZE;
        for mip in 0..MIP_LEVELS {
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture,
                    mip_level: mip,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer_idx as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &current,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * dim),
                    rows_per_image: Some(dim),
                },
                wgpu::Extent3d {
                    width: dim,
                    height: dim,
                    depth_or_array_layers: 1,
                },
            );
            if mip + 1 < MIP_LEVELS {
                current = downsample_rgba_2x(&current, dim);
                dim = (dim / 2).max(1);
            }
        }
    }
}

pub fn create_block_texture_array(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (wgpu::Texture, wgpu::TextureView, wgpu::Sampler) {
    let layers = generate_all_texture_layers_rgba();
    let size = wgpu::Extent3d {
        width: TEX_SIZE,
        height: TEX_SIZE,
        depth_or_array_layers: NUM_TEX_LAYERS,
    };

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("block_texture_array"),
        size,
        mip_level_count: MIP_LEVELS,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    upload_block_layers(&texture, queue, &layers);

    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("block_texture_array_view"),
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        array_layer_count: Some(NUM_TEX_LAYERS),
        ..Default::default()
    });

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("block_sampler"),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        address_mode_w: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    (texture, view, sampler)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_procedural_texture_layers_non_empty() {
        let layers = generate_all_texture_layers_rgba();
        assert_eq!(layers.len(), NUM_TEX_LAYERS as usize);
        assert_eq!(layers.len(), 48);
        for layer in &layers {
            assert_eq!(layer.len(), (TEX_SIZE * TEX_SIZE * 4) as usize);
            let (pixels, remainder) = layer.as_chunks::<4>();
            assert!(remainder.is_empty());
            assert!(pixels.iter().all(|px| px[3] == 255));
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
        let authored_new = [16usize, 17, 18, 19, 20, 21, 22];

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

        // Every new layer is real art, not a flat color.
        for index in authored_new {
            let (pixels, _) = layers[index].as_chunks::<4>();
            let first = &pixels[0];
            assert!(
                pixels.iter().any(|px| px != first),
                "layer {index} must not be a flat color"
            );
        }
    }

    #[test]
    fn test_reserved_texture_layers_are_distinct_opaque_placeholders() {
        let layers = generate_all_texture_layers_rgba();
        let authorable = 23usize;
        assert_eq!(NUM_TEX_LAYERS as usize, 48);

        // Block-reserved 23..32 and spare 45..48 both use the reserved rule;
        // neither range may alias authored art or mob art.
        let reserved: Vec<usize> = (authorable..MOB_LAYER_BASE as usize)
            .chain(MOB_SPARE_LAYER_BASE as usize..NUM_TEX_LAYERS as usize)
            .collect();
        assert_eq!(reserved.len(), 9 + 3);

        for &index in &reserved {
            for authored in 0..authorable {
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
            for authored in 0..23usize {
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
}
