//! RGBA byte helpers shared by the renderer and the pack resampler.

/// Halve a square RGBA image with a 2x box average (edge-clamped).
/// Shared by the texture-array mip chain and the pack resample path so both
/// use one averaging rule.
///
/// `src_dim` must be non-zero and `src` must hold `src_dim * src_dim * 4`
/// bytes; callers validate both before reaching this seam.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_by_two_averages_each_rgba_channel() {
        // Four 2x2 pixels with distinct channels; truncating average is 25/50/etc.
        let src = [
            10u8, 20, 30, 40, // (0, 0)
            30, 40, 50, 60, // (1, 0)
            40, 60, 80, 100, // (0, 1)
            20, 80, 40, 100, // (1, 1)
        ];
        let dst = downsample_rgba_2x(&src, 2);
        assert_eq!(dst, vec![25, 50, 50, 75]);
    }

    #[test]
    fn odd_one_pixel_source_clamps_every_sample_to_the_edge() {
        // src_dim = 1 halves to one pixel; all four 2x2 taps clamp to (0, 0),
        // so the average is the single source pixel unchanged.
        let src = [7u8, 8, 9, 10];
        assert_eq!(downsample_rgba_2x(&src, 1), src.to_vec());
    }
}
