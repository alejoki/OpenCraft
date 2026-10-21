//! GPU-free, never-fail sound overrides for texture/sound packs.
//!
//! A pack may ship up to four audio files (one per [`SoundSlot`]) flat in its
//! root, next to the texture layers. This loader mirrors the texture loader's
//! contract: it always returns a complete `[SoundBuffer; 4]` set and never
//! returns `Err`, never panics, and never touches an audio device. Every file
//! problem degrades to that slot's synthesized default plus one structured
//! warning naming the bare file, so no audio file state can block launch.
//!
//! The loader stays a separate entry point (rather than living inside
//! [`crate::pack::LoadedPack`]) because `SoundBuffer` carries `f32` samples and
//! `LoadedPack` derives `Eq`.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use rodio::Source;

use crate::audio::{default_buffers, SoundBuffer, SoundSlot};
use crate::settings::SettingsWarning;

/// Largest accepted sound file, in bytes. Checked before decoding so a
/// pathological file can never be turned into a huge in-memory sample buffer.
pub const SOUND_BYTE_CAP: u64 = 1_048_576;

/// Accepted audio extensions in resolution priority order. The first existing
/// `<slot>.<ext>` file wins; when several exist only the highest priority one
/// is read.
pub const SOUND_EXTENSIONS: [&str; 5] = ["wav", "ogg", "flac", "mp3", "m4a"];

/// Resolve every sound slot from `dir`, falling back per file.
///
/// `dir` is the pack root (the same folder that holds `pack.toml` and the
/// texture layers). Missing files are legal and silent: the slot keeps its
/// synthesized default. A file that exists but is unreadable, larger than
/// [`SOUND_BYTE_CAP`], fails to decode, or decodes to zero samples produces
/// exactly one warning whose `field` is the bare file name.
pub fn load_sound_overrides(dir: &Path) -> ([SoundBuffer; 4], Vec<SettingsWarning>) {
    let mut buffers = default_buffers();
    let mut warnings = Vec::new();

    for slot in SoundSlot::ALL {
        let Some((file, path)) = first_existing_file(dir, slot) else {
            // Partial packs are legal: keep the synthesized default silently.
            continue;
        };

        match read_and_decode(&path) {
            Ok(buffer) => buffers[slot.index()] = buffer,
            Err(reason) => warnings.push(SettingsWarning {
                field: file,
                reason,
            }),
        }
    }

    (buffers, warnings)
}

/// First existing `<slot>.<ext>` candidate, in [`SOUND_EXTENSIONS`] order.
fn first_existing_file(dir: &Path, slot: SoundSlot) -> Option<(String, PathBuf)> {
    SOUND_EXTENSIONS.iter().find_map(|extension| {
        let file = format!("{}.{extension}", slot.name());
        let path = dir.join(&file);
        path.is_file().then_some((file, path))
    })
}

/// Read one candidate and decode it with rodio's real (symphonia) decoder.
///
/// The size cap is enforced from metadata before the read and re-checked on
/// the actual byte length, so a file swapped underneath us cannot slip past.
fn read_and_decode(path: &Path) -> Result<SoundBuffer, String> {
    let metadata =
        std::fs::metadata(path).map_err(|err| format!("failed to inspect sound file: {err}"))?;
    if metadata.len() > SOUND_BYTE_CAP {
        return Err(oversized_reason(metadata.len()));
    }

    let bytes =
        std::fs::read(path).map_err(|err| format!("failed to read sound file: {err}"))?;
    if bytes.len() as u64 > SOUND_BYTE_CAP {
        return Err(oversized_reason(bytes.len() as u64));
    }

    decode_sound(bytes)
}

fn oversized_reason(len: u64) -> String {
    format!("sound file is {len} bytes, the limit is {SOUND_BYTE_CAP} bytes")
}

/// Decode arbitrary bytes, preserving the source's channel count and rate.
fn decode_sound(bytes: Vec<u8>) -> Result<SoundBuffer, String> {
    let decoder = rodio::Decoder::try_from(Cursor::new(bytes))
        .map_err(|err| format!("failed to decode sound file: {err}"))?;

    // Capture format metadata before the iterator is consumed.
    let channels = decoder.channels().get();
    let sample_rate = decoder.sample_rate().get();
    let samples: Vec<f32> = decoder.collect();

    if samples.is_empty() {
        // A header-only/zero-frame file is not worth handing to the mixer.
        return Err("sound file decoded to zero samples".to_string());
    }

    Ok(SoundBuffer::new(samples, channels, sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Canonical 44-byte-header RIFF/WAVE PCM16 fixture writer.
    fn wav_bytes(channels: u16, sample_rate: u32, samples: &[i16]) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let block_align = channels * 2;
        let byte_rate = sample_rate * u32::from(block_align);
        let mut out = Vec::with_capacity(44 + data_len as usize);

        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        out
    }

    /// Deterministic, clearly non-silent PCM16 payload.
    fn ramp(len: usize) -> Vec<i16> {
        (0..len)
            .map(|i| ((i.wrapping_mul(517) % 20_000) as i16) - 10_000)
            .collect()
    }

    fn pack_dir(root: &tempfile::TempDir) -> PathBuf {
        let dir = root.path().join("sound-pack");
        fs::create_dir_all(&dir).expect("create pack dir");
        dir
    }

    fn write_sound(dir: &Path, file: &str, bytes: &[u8]) {
        fs::write(dir.join(file), bytes).expect("write sound fixture");
    }

    fn warning_fields(warnings: &[SettingsWarning]) -> Vec<&str> {
        warnings.iter().map(|w| w.field.as_str()).collect()
    }

    #[test]
    fn valid_overrides_decode_with_their_own_format_metadata() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);

        let mono = ramp(64);
        write_sound(&dir, "break.wav", &wav_bytes(1, 22_050, &mono));
        let stereo = ramp(128);
        write_sound(&dir, "place.wav", &wav_bytes(2, 44_100, &stereo));

        let (buffers, warnings) = load_sound_overrides(&dir);
        let defaults = default_buffers();

        assert!(warnings.is_empty(), "{warnings:?}");

        // break.wav: mono 22 050 Hz round-trips through SoundBuffer.
        assert_eq!(buffers[0].channels(), 1);
        assert_eq!(buffers[0].sample_rate(), 22_050);
        assert_eq!(buffers[0].samples().len(), mono.len());
        assert!(
            (buffers[0].samples()[0] - f32::from(mono[0]) / 32_768.0).abs() < 1e-4,
            "decoded sample should match the authored PCM16 value"
        );
        assert_ne!(
            buffers[0].samples(),
            defaults[0].samples(),
            "pack audio must replace the synth default"
        );

        // place.wav: the stereo 44 100 Hz case.
        assert_eq!(buffers[1].channels(), 2);
        assert_eq!(buffers[1].sample_rate(), 44_100);
        assert_eq!(buffers[1].samples().len(), stereo.len());
        assert_ne!(buffers[1].samples(), defaults[1].samples());

        // Unprovided slots stay silent defaults without warnings.
        assert_eq!(buffers[2], defaults[2]);
        assert_eq!(buffers[3], defaults[3]);
    }

    #[test]
    fn every_slot_maps_its_own_file_name_and_format() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);

        let cases = [
            ("break.wav", 1u16, 22_050u32),
            ("place.wav", 2, 44_100),
            ("mine_complete.wav", 1, 8_000),
            ("ui_click.wav", 2, 48_000),
        ];
        for (file, channels, rate) in cases {
            let samples = ramp(32 * usize::from(channels));
            write_sound(&dir, file, &wav_bytes(channels, rate, &samples));
        }

        let (buffers, warnings) = load_sound_overrides(&dir);
        assert!(warnings.is_empty(), "{warnings:?}");
        for (index, (_, channels, rate)) in cases.iter().enumerate() {
            assert_eq!(buffers[index].channels(), *channels, "slot {index} channels");
            assert_eq!(
                buffers[index].sample_rate(),
                *rate,
                "slot {index} sample rate"
            );
        }
    }

    #[test]
    fn extension_priority_prefers_wav_over_ogg() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);

        // The .wav fixture is mono 22 050 Hz; the .ogg fixture is a valid
        // container whose decoded format is distinguishable (stereo 44 100).
        write_sound(&dir, "break.wav", &wav_bytes(1, 22_050, &ramp(32)));
        write_sound(&dir, "break.ogg", &wav_bytes(2, 44_100, &ramp(64)));

        let (buffers, warnings) = load_sound_overrides(&dir);

        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(buffers[0].channels(), 1);
        assert_eq!(buffers[0].sample_rate(), 22_050);
    }

    #[test]
    fn corrupt_file_warns_and_keeps_the_synth_default() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);
        write_sound(&dir, "break.wav", b"this is definitely not an audio file");

        let (buffers, warnings) = load_sound_overrides(&dir);

        assert_eq!(warning_fields(&warnings), vec!["break.wav"]);
        assert!(!warnings[0].reason.is_empty());
        assert_eq!(buffers, default_buffers());
    }

    #[test]
    fn oversized_file_warns_and_keeps_the_synth_default() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);
        let oversized = vec![0u8; SOUND_BYTE_CAP as usize + 1];
        write_sound(&dir, "break.wav", &oversized);

        let (buffers, warnings) = load_sound_overrides(&dir);

        assert_eq!(warning_fields(&warnings), vec!["break.wav"]);
        assert!(
            warnings[0].reason.contains("1048576"),
            "reason names the cap: {}",
            warnings[0].reason
        );
        assert_eq!(buffers, default_buffers());
    }

    #[test]
    fn file_exactly_at_the_cap_is_not_rejected_by_size() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);
        // A cap-sized junk file must still fail as a decode error (not as
        // "too large"), proving the boundary is inclusive.
        write_sound(&dir, "break.wav", &vec![0u8; SOUND_BYTE_CAP as usize]);

        let (buffers, warnings) = load_sound_overrides(&dir);

        assert_eq!(warning_fields(&warnings), vec!["break.wav"]);
        assert!(
            !warnings[0].reason.contains("limit is"),
            "cap-sized file must not be classified as oversized: {}",
            warnings[0].reason
        );
        assert_eq!(buffers, default_buffers());
    }

    #[test]
    fn header_only_wav_warns_and_keeps_the_synth_default() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);
        write_sound(&dir, "break.wav", &wav_bytes(1, 22_050, &[]));

        let (buffers, warnings) = load_sound_overrides(&dir);

        assert_eq!(warning_fields(&warnings), vec!["break.wav"]);
        assert_eq!(buffers, default_buffers());
    }

    #[test]
    fn missing_files_and_empty_dirs_are_silent_defaults() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);

        let (buffers, warnings) = load_sound_overrides(&dir);
        assert!(warnings.is_empty());
        assert_eq!(buffers, default_buffers());

        // A missing pack folder is equally legal.
        let (missing_buffers, missing_warnings) =
            load_sound_overrides(&root.path().join("absent"));
        assert!(missing_warnings.is_empty());
        assert_eq!(missing_buffers, default_buffers());
    }

    #[test]
    fn first_existing_extension_is_chosen_even_when_it_is_broken() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);
        write_sound(&dir, "break.wav", b"corrupt");
        write_sound(&dir, "break.ogg", &wav_bytes(1, 22_050, &ramp(32)));

        let (buffers, warnings) = load_sound_overrides(&dir);

        // Only the highest-priority candidate is read: the corrupt .wav wins
        // resolution, so it warns and falls back rather than silently moving on.
        assert_eq!(warning_fields(&warnings), vec!["break.wav"]);
        assert_eq!(buffers, default_buffers());
    }
}
