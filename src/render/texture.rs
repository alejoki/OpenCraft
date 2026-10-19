pub const TEX_SIZE: u32 = 16;
pub const NUM_TEX_LAYERS: u32 = 14;
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

pub fn generate_all_texture_layers_rgba() -> Vec<Vec<u8>> {
    vec![
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
    ]
}

fn downsample_rgba_2x(src: &[u8], src_dim: u32) -> Vec<u8> {
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

pub fn create_block_texture_array(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (wgpu::TextureView, wgpu::Sampler) {
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

    for (layer_idx, base_rgba) in layers.into_iter().enumerate() {
        let mut current = base_rgba;
        let mut dim = TEX_SIZE;
        for mip in 0..MIP_LEVELS {
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &texture,
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

    (view, sampler)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_procedural_texture_layers_non_empty() {
        let layers = generate_all_texture_layers_rgba();
        assert_eq!(layers.len(), 14);
        for layer in &layers {
            assert_eq!(layer.len(), 16 * 16 * 4);
            assert!(layer.chunks_exact(4).all(|px| px[3] == 255));
        }
    }
}
