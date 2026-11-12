//! Pure PNG decoding to the RGBA byte format the texture array consumes.
//!
//! Decoding never touches the GPU. Every failure is returned as an `Err`
//! string naming what went wrong; callers turn that into a per-file fallback
//! plus a structured warning so a bad pack can never block loading.

use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;

use crate::pack::asset::PACK_ASSET_BYTE_CAP;

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
/// `max_side` bounds the header dimensions before any pixel buffer is
/// allocated; `None` leaves the dimensions unbounded. The caller validates
/// squareness and the 16..=256 range. No resampling happens here.
pub fn decode_png_rgba(path: &Path, max_side: Option<u32>) -> Result<DecodedPng, String> {
    let bytes = read_file_bytes(path)?;
    decode_bytes(&bytes, &path.display().to_string(), max_side)
}

/// Decode a PNG from memory for use as a pack asset.
///
/// `label` names the authored path in error messages. This is the bytes-based
/// half of [`decode_png_rgba`]: v2 assets are read through the confinement
/// layer first and then decoded here, so loading never re-opens a path that
/// was not already validated.
pub fn decode_png_rgba_bytes(
    bytes: &[u8],
    label: &str,
    max_side: Option<u32>,
) -> Result<DecodedPng, String> {
    decode_bytes(bytes, label, max_side)
}

/// Decode a PNG for use as a pack logo, preserving its native size.
///
/// Rejects zero-sized and larger-than-1024-per-side images.
pub fn decode_png_native(path: &Path) -> Result<DecodedPng, String> {
    let bytes = read_file_bytes(path)?;
    decode_bytes(&bytes, &path.display().to_string(), Some(MAX_LOGO_SIDE))
}

/// Read one PNG file, rejecting non-regular files and oversized reads.
///
/// The metadata checks run before `File::open` so a FIFO or an over-cap file
/// can never block or be buffered. The actual read is capped and re-checked
/// so a file that grows or reports a stale length still fails closed.
fn read_file_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|err| format!("failed to open {}: {err}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if metadata.len() > PACK_ASSET_BYTE_CAP {
        return Err(format!(
            "{} is {} bytes, larger than the {} byte limit",
            path.display(),
            metadata.len(),
            PACK_ASSET_BYTE_CAP
        ));
    }

    let file =
        File::open(path).map_err(|err| format!("failed to open {}: {err}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(PACK_ASSET_BYTE_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    if bytes.len() as u64 > PACK_ASSET_BYTE_CAP {
        return Err(format!(
            "{} is {} bytes, larger than the {} byte limit",
            path.display(),
            bytes.len(),
            PACK_ASSET_BYTE_CAP
        ));
    }
    Ok(bytes)
}

fn decode_bytes(bytes: &[u8], label: &str, max_side: Option<u32>) -> Result<DecodedPng, String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());

    let mut reader = decoder
        .read_info()
        .map_err(|err| format!("failed to read PNG header {label}: {err}"))?;

    let (width, height) = {
        let info = reader.info();
        (info.width, info.height)
    };

    if let Some(limit) = max_side {
        if width == 0 || height == 0 {
            return Err(format!(
                "{label} has an empty image size ({width}x{height})"
            ));
        }
        if width > limit || height > limit {
            return Err(format!(
                "{label} is {width}x{height}, larger than the {limit} px limit"
            ));
        }
    }

    let mut buf = vec![0u8; reader.output_buffer_size()];
    let output = reader
        .next_frame(&mut buf)
        .map_err(|err| format!("failed to decode PNG {label}: {err}"))?;

    if output.color_type == png::ColorType::Indexed {
        return Err(format!(
            "{label} still uses an indexed color type after normalization"
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

/// Build a minimal PNG whose IHDR declares `width`x`height` but whose pixel
/// data is empty. The empty `IDAT` is load-bearing: the decoder stops reading
/// at the first IDAT, so the dimensions reach the caller's guard instead of a
/// `MissingImageData` header error.
#[cfg(test)]
pub(crate) fn png_bomb_bytes(width: u32, height: u32) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in bytes {
            crc ^= byte as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + data.len());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc_input = Vec::with_capacity(4 + data.len());
        crc_input.extend_from_slice(kind);
        crc_input.extend_from_slice(data);
        out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
        out
    }

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(6); // color type: RGBA
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    out.extend_from_slice(&chunk(b"IHDR", &ihdr));
    out.extend_from_slice(&chunk(b"IDAT", &[]));
    out.extend_from_slice(&chunk(b"IEND", &[]));
    out
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

        let decoded = decode_png_rgba(&path, None).unwrap();
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

        let decoded = decode_png_rgba(&path, None).unwrap();
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

        let decoded = decode_png_rgba(&path, None).unwrap();
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

        let decoded = decode_png_rgba(&path, None).unwrap();
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

        let err = decode_png_rgba(&path, None).unwrap_err();
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

        let err = decode_png_rgba(&path, None).unwrap_err();
        assert!(
            err.contains("truncated.png"),
            "error should name the file: {err}"
        );
    }

    #[test]
    fn missing_file_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_path(&dir, "missing.png");

        let err = decode_png_rgba(&path, None).unwrap_err();
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

        let decoded = decode_png_rgba(&path, None).unwrap();
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

        let decoded = decode_png_rgba(&path, None).unwrap();
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

    #[test]
    fn oversized_ihdr_is_rejected_before_allocation() {
        let bomb = png_bomb_bytes(30000, 30000);
        assert!(bomb.len() < 4096, "bomb is {} bytes", bomb.len());

        let err = decode_png_rgba_bytes(&bomb, "bomb.png", Some(256)).unwrap_err();
        assert!(
            err.contains("30000x30000"),
            "error should report the IHDR size: {err}"
        );
        assert!(err.contains("256"), "error should mention the limit: {err}");
    }

    #[test]
    fn oversized_png_file_is_rejected_before_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_path(&dir, "stone.png");
        fs::write(&path, vec![0u8; PACK_ASSET_BYTE_CAP as usize + 1]).unwrap();

        let err = decode_png_rgba(&path, Some(256)).unwrap_err();
        assert!(
            err.contains("stone.png"),
            "error should name the file: {err}"
        );
        assert!(
            err.contains("1048576"),
            "error should mention the byte cap: {err}"
        );
    }
}
