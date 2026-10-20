//! Pure PNG decoding to the RGBA byte format the texture array consumes.
//!
//! Decoding never touches the GPU. Every failure is returned as an `Err`
//! string naming what went wrong; callers turn that into a per-file fallback
//! plus a structured warning so a bad pack can never block loading.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

/// Largest accepted logo side length, in pixels.
///
/// Layer textures are validated separately (16..=256 square) by the loader.
const MAX_LOGO_SIDE: u32 = 1024;

/// A decoded PNG in tightly packed RGBA8 (four bytes per pixel, row-major).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedPng {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Convert raw decoded PNG bytes of `color_type` into RGBA8.
///
/// `pixels` is the total pixel count. Panics if `bytes` does not match the
/// channel layout of `color_type`; that can only happen on a decoder bug.
pub fn to_rgba(bytes: &[u8], color_type: png::ColorType, pixels: usize) -> Vec<u8> {
    let channels = match color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => {
            panic!("to_rgba: indexed pixels must be expanded before conversion")
        }
    };
    assert_eq!(
        bytes.len(),
        pixels * channels,
        "to_rgba: expected {} bytes for {} {:?} pixels, got {}",
        pixels * channels,
        pixels,
        color_type,
        bytes.len()
    );

    match color_type {
        png::ColorType::Rgba => bytes.to_vec(),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(pixels * 4);
            for px in bytes.as_chunks::<3>().0 {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity(pixels * 4);
            for &g in bytes {
                out.extend_from_slice(&[g, g, g, 255]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity(pixels * 4);
            for px in bytes.as_chunks::<2>().0 {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
            out
        }
        png::ColorType::Indexed => unreachable!("indexed pixels are rejected above"),
    }
}

/// Decode a PNG for use as a block layer texture.
///
/// The image may have any dimensions; the caller validates squareness and the
/// 16..=256 range. No resampling happens here.
pub fn decode_png_rgba(path: &Path) -> Result<DecodedPng, String> {
    decode(path, None)
}

/// Decode a PNG for use as a pack logo, preserving its native size.
///
/// Rejects zero-sized and larger-than-1024-per-side images.
pub fn decode_png_native(path: &Path) -> Result<DecodedPng, String> {
    decode(path, Some(MAX_LOGO_SIDE))
}

fn decode(path: &Path, max_side: Option<u32>) -> Result<DecodedPng, String> {
    let file =
        File::open(path).map_err(|err| format!("failed to open {}: {err}", path.display()))?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8());

    let mut reader = decoder
        .read_info()
        .map_err(|err| format!("failed to read PNG header {}: {err}", path.display()))?;

    let (width, height) = {
        let info = reader.info();
        (info.width, info.height)
    };

    if let Some(limit) = max_side {
        if width == 0 || height == 0 {
            return Err(format!(
                "{} has an empty image size ({width}x{height})",
                path.display()
            ));
        }
        if width > limit || height > limit {
            return Err(format!(
                "{} is {width}x{height}, larger than the {limit} px logo limit",
                path.display()
            ));
        }
    }

    let mut buf = vec![0u8; reader.output_buffer_size()];
    let output = reader
        .next_frame(&mut buf)
        .map_err(|err| format!("failed to decode PNG {}: {err}", path.display()))?;

    if output.color_type == png::ColorType::Indexed {
        return Err(format!(
            "{} still uses an indexed color type after normalization",
            path.display()
        ));
    }

    let bytes = &buf[..output.buffer_size()];
    let pixels = (output.width as usize) * (output.height as usize);
    let rgba = to_rgba(bytes, output.color_type, pixels);

    Ok(DecodedPng {
        rgba,
        width: output.width,
        height: output.height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn write_png(path: &Path, color_type: png::ColorType, width: u32, height: u32, data: &[u8]) {
        let file = File::create(path).expect("create fixture png");
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(color_type);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("write png header");
        writer.write_image_data(data).expect("write png image data");
    }

    fn fixture_path(dir: &tempfile::TempDir, name: &str) -> PathBuf {
        dir.path().join(name)
    }

    #[test]
    fn rgba_round_trips_with_pixel_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_path(&dir, "rgba32.png");
        let mut data = vec![0u8; 32 * 32 * 4];
        for (i, px) in data.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            px.copy_from_slice(&[
                (i % 256) as u8,
                (i * 3 % 256) as u8,
                (255 - (i % 256)) as u8,
                if i % 2 == 0 { 255 } else { 7 },
            ]);
        }
        write_png(&path, png::ColorType::Rgba, 32, 32, &data);

        let decoded = decode_png_rgba(&path).unwrap();
        assert_eq!(decoded.width, 32);
        assert_eq!(decoded.height, 32);
        assert_eq!(decoded.rgba.len(), 32 * 32 * 4);
        assert_eq!(decoded.rgba, data);
    }

    #[test]
    fn grayscale_replicates_channel_and_sets_alpha() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_path(&dir, "gray16.png");
        let data: Vec<u8> = (0..16 * 16).map(|i| (i % 251) as u8).collect();
        write_png(&path, png::ColorType::Grayscale, 16, 16, &data);

        let decoded = decode_png_rgba(&path).unwrap();
        assert_eq!(decoded.rgba.len(), 16 * 16 * 4);
        for (i, &g) in data.iter().enumerate() {
            assert_eq!(
                &decoded.rgba[i * 4..i * 4 + 4],
                &[g, g, g, 255],
                "pixel {i}"
            );
        }
    }

    #[test]
    fn rgb_gets_opaque_alpha() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_path(&dir, "rgb16.png");
        let data: Vec<u8> = (0..16 * 16 * 3).map(|i| (i * 7 % 256) as u8).collect();
        write_png(&path, png::ColorType::Rgb, 16, 16, &data);

        let decoded = decode_png_rgba(&path).unwrap();
        assert_eq!(decoded.rgba.len(), 16 * 16 * 4);
        for (i, px) in data.as_chunks::<3>().0.iter().enumerate() {
            assert_eq!(
                &decoded.rgba[i * 4..i * 4 + 4],
                &[px[0], px[1], px[2], 255],
                "pixel {i}"
            );
        }
    }

    #[test]
    fn grayscale_alpha_is_expanded() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_path(&dir, "graya4.png");
        let data = vec![10, 20, 30, 40, 50, 60, 70, 80];
        write_png(&path, png::ColorType::GrayscaleAlpha, 2, 2, &data);

        let decoded = decode_png_rgba(&path).unwrap();
        assert_eq!(
            decoded.rgba,
            vec![10, 10, 10, 20, 30, 30, 30, 40, 50, 50, 50, 60, 70, 70, 70, 80]
        );
    }

    #[test]
    fn corrupt_bytes_return_err() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_path(&dir, "corrupt.png");
        fs::write(&path, b"this is definitely not a png file").unwrap();

        let err = decode_png_rgba(&path).unwrap_err();
        assert!(
            err.contains("corrupt.png"),
            "error should name the file: {err}"
        );
    }

    #[test]
    fn truncated_png_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_path(&dir, "truncated.png");
        let data = vec![128u8; 32 * 32 * 4];
        write_png(&path, png::ColorType::Rgba, 32, 32, &data);

        let mut bytes = fs::read(&path).unwrap();
        bytes.truncate(bytes.len() / 2);
        fs::write(&path, &bytes).unwrap();

        let err = decode_png_rgba(&path).unwrap_err();
        assert!(
            err.contains("truncated.png"),
            "error should name the file: {err}"
        );
    }

    #[test]
    fn missing_file_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_path(&dir, "missing.png");

        let err = decode_png_rgba(&path).unwrap_err();
        assert!(
            err.contains("missing.png"),
            "error should name the file: {err}"
        );
    }

    #[test]
    fn non_square_reports_actual_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_path(&dir, "wide.png");
        let data = vec![200u8; 32 * 16 * 4];
        write_png(&path, png::ColorType::Rgba, 32, 16, &data);

        let decoded = decode_png_rgba(&path).unwrap();
        assert_eq!(decoded.width, 32);
        assert_eq!(decoded.height, 16);
        assert_eq!(decoded.rgba.len(), 32 * 16 * 4);
    }

    #[test]
    fn twenty_four_square_keeps_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_path(&dir, "24.png");
        let data = vec![33u8; 24 * 24 * 4];
        write_png(&path, png::ColorType::Rgba, 24, 24, &data);

        let decoded = decode_png_rgba(&path).unwrap();
        assert_eq!(decoded.width, 24);
        assert_eq!(decoded.height, 24);
        assert_eq!(decoded.rgba.len(), 24 * 24 * 4);
        assert!(decoded.rgba.iter().all(|&b| b == 33));
    }

    #[test]
    fn oversized_logo_is_rejected_by_native_decoder() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_path(&dir, "huge_logo.png");
        let data = vec![255u8; 4096 * 16 * 4];
        write_png(&path, png::ColorType::Rgba, 4096, 16, &data);

        let err = decode_png_native(&path).unwrap_err();
        assert!(
            err.contains("1024"),
            "error should mention the logo bound: {err}"
        );
    }

    #[test]
    fn native_logo_keeps_aspect_ratio() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_path(&dir, "logo.png");
        let data = vec![9u8; 64 * 32 * 4];
        write_png(&path, png::ColorType::Rgba, 64, 32, &data);

        let decoded = decode_png_native(&path).unwrap();
        assert_eq!(decoded.width, 64);
        assert_eq!(decoded.height, 32);
        assert_eq!(decoded.rgba.len(), 64 * 32 * 4);
    }

    #[test]
    #[should_panic(expected = "to_rgba: expected")]
    fn to_rgba_rejects_wrong_length() {
        let _ = to_rgba(&[0, 0, 0], png::ColorType::Rgba, 2);
    }
}
