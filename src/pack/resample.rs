//! Pure RGBA resampling for pack textures.
//!
//! Pack textures are authored as square PNGs between 16x16 and 256x256 and
//! must land on the fixed 16x16 layer format the block texture array expects.
//! Power-of-two sizes reuse the same `downsample_rgba_2x` helper the mip
//! chain uses (one function, one averaging rule); other square sizes go
//! through an integer box filter with the same truncating-average semantics.
//! No upscaling: the caller validates the range.

use crate::render::texture::downsample_rgba_2x;

const TARGET_DIM: u32 = 16;

/// Resample a square RGBA image to exactly 16x16 RGBA bytes.
///
/// `src_dim` must be the square side length and `src` must hold
/// `src_dim * src_dim * 4` bytes. Range validation (16..=256) belongs to the
/// decode step; this function assumes already-validated input.
pub fn resample_to_16(src: &[u8], src_dim: u32) -> Vec<u8> {
    let expected = (src_dim as usize) * (src_dim as usize) * 4;
    assert_eq!(
        src.len(),
        expected,
        "resample_to_16: expected {expected} bytes for {src_dim}x{src_dim} RGBA, got {}",
        src.len()
    );
    assert!(src_dim >= TARGET_DIM, "resample_to_16: no upscaling");

    if src_dim == TARGET_DIM {
        return src.to_vec();
    }

    if src_dim.is_power_of_two() {
        let mut current = src.to_vec();
        let mut dim = src_dim;
        while dim > TARGET_DIM {
            current = downsample_rgba_2x(&current, dim);
            dim /= 2;
        }
        return current;
    }

    box_resample_to_16(src, src_dim)
}

/// General integer box filter for square sizes that are not powers of two.
///
/// Each destination pixel averages the source box it covers. Box edges use
/// integer division of the destination index (`x * src_dim / 16`) so the
/// boxes tile the source exactly; the last box is clamped to the source
/// edge. Averages use truncating integer division, matching the 2x helper.
fn box_resample_to_16(src: &[u8], src_dim: u32) -> Vec<u8> {
    let mut dst = vec![0u8; (TARGET_DIM * TARGET_DIM * 4) as usize];

    for dy in 0..TARGET_DIM {
        let sy0 = dy * src_dim / TARGET_DIM;
        let sy1 = (((dy + 1) * src_dim / TARGET_DIM).max(sy0 + 1)).min(src_dim);
        for dx in 0..TARGET_DIM {
            let sx0 = dx * src_dim / TARGET_DIM;
            let sx1 = (((dx + 1) * src_dim / TARGET_DIM).max(sx0 + 1)).min(src_dim);

            let mut sum = [0u32; 4];
            let mut count = 0u32;
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let sidx = ((sy.min(src_dim - 1) * src_dim + sx.min(src_dim - 1)) * 4) as usize;
                    for c in 0..4 {
                        sum[c] += src[sidx + c] as u32;
                    }
                    count += 1;
                }
            }

            let didx = ((dy * TARGET_DIM + dx) * 4) as usize;
            for c in 0..4 {
                dst[didx + c] = (sum[c] / count) as u8;
            }
        }
    }

    dst
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::texture::{downsample_rgba_2x, TEX_SIZE};

    fn solid_rgba(dim: u32, px: [u8; 4]) -> Vec<u8> {
        px.iter()
            .copied()
            .cycle()
            .take((dim * dim * 4) as usize)
            .collect()
    }

    /// Deterministic gradient used for hand-computed box-filter checks.
    /// red = x*10, green = y*10, blue = (x+y)*5, alpha = 255.
    fn gradient_24() -> Vec<u8> {
        let mut data = Vec::with_capacity(24 * 24 * 4);
        for y in 0..24u32 {
            for x in 0..24u32 {
                data.extend_from_slice(&[
                    (x * 10) as u8,
                    (y * 10) as u8,
                    ((x + y) * 5) as u8,
                    255,
                ]);
            }
        }
        data
    }

    fn pixel(buf: &[u8], dim: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * dim + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    #[test]
    fn identity_at_16_is_byte_identical() {
        let src = gradient_24();
        let src16 = resample_to_16(&src[..16 * 16 * 4], 16);
        assert_eq!(src16, &src[..16 * 16 * 4]);
        assert_eq!(src16.len(), (TEX_SIZE * TEX_SIZE * 4) as usize);
    }

    #[test]
    fn uniform_32_downsamples_to_same_uniform_16() {
        let src = solid_rgba(32, [40, 90, 200, 255]);
        let out = resample_to_16(&src, 32);
        assert_eq!(out.len(), (TEX_SIZE * TEX_SIZE * 4) as usize);
        assert!(out.as_chunks::<4>().0.iter().all(|px| px == &[40, 90, 200, 255]));
    }

    #[test]
    fn pot_32_matches_repeated_downsample_helper_chain() {
        let src32: Vec<u8> = (0..32u32)
            .flat_map(|y| (0..32u32).flat_map(move |x| {
                [
                    (x * 7) as u8,
                    (y * 7) as u8,
                    ((x + y) * 3) as u8,
                    200,
                ]
            }))
            .collect();
        let direct = downsample_rgba_2x(&src32, 32);
        assert_eq!(resample_to_16(&src32, 32), direct);
        assert_eq!(direct.len(), (16 * 16 * 4) as usize);
    }

    #[test]
    fn pot_64_chain_spot_check() {
        let src32 = solid_rgba(32, [10, 20, 30, 255]);
        // 2x2 replication of each 32x32 pixel gives a uniform 64x64 image.
        let mut src64 = Vec::with_capacity(64 * 64 * 4);
        for y in 0..64u32 {
            for x in 0..64u32 {
                src64.extend_from_slice(&src32[(((y / 2) * 32 + (x / 2)) * 4) as usize..][..4]);
            }
        }
        let out = resample_to_16(&src64, 64);
        assert_eq!(out.len(), 16 * 16 * 4);
        assert!(out.as_chunks::<4>().0.iter().all(|px| px == &[10, 20, 30, 255]));
        assert_eq!(pixel(&out, 16, 0, 0), [10, 20, 30, 255]);
        assert_eq!(pixel(&out, 16, 15, 15), [10, 20, 30, 255]);
    }

    #[test]
    fn non_pot_24_matches_hand_computed_pixels() {
        let src = gradient_24();
        let out = resample_to_16(&src, 24);
        assert_eq!(out.len(), 16 * 16 * 4);

        // Dest (0,0): box covers source (0,0) only.
        assert_eq!(pixel(&out, 16, 0, 0), [0, 0, 0, 255]);
        // Dest (1,1): box covers source x 1..3, y 1..3.
        // red avg (10+20)/2=15, green 15, blue (10+15+15+20)/4=15.
        assert_eq!(pixel(&out, 16, 1, 1), [15, 15, 15, 255]);
        // Dest (15,15): clamped edge box covering x 22..24, y 22..24
        // (non-multiple ratio). red (220+230)/2=225, green 225,
        // blue (220+225+225+230)/4=225.
        assert_eq!(pixel(&out, 16, 15, 15), [225, 225, 225, 255]);
    }

    #[test]
    fn non_pot_48_uniform_stays_uniform() {
        let src = solid_rgba(48, [7, 9, 11, 13]);
        let out = resample_to_16(&src, 48);
        assert_eq!(out.len(), 16 * 16 * 4);
        assert!(out.as_chunks::<4>().0.iter().all(|px| px == &[7, 9, 11, 13]));
    }

    #[test]
    #[should_panic(expected = "expected")]
    fn wrong_length_input_panics() {
        let src = solid_rgba(16, [1, 2, 3, 4]);
        let _ = resample_to_16(&src[..src.len() - 1], 16);
    }

    #[test]
    #[should_panic(expected = "no upscaling")]
    fn upscale_request_panics() {
        let src = solid_rgba(8, [1, 2, 3, 4]);
        let _ = resample_to_16(&src, 8);
    }
}
