//! Sound playback: a tiny rodio-backed engine plus deterministic synthesized
//! fallback buffers.
//!
//! The engine is deliberately fail-soft: if no output device can be opened
//! (headless CI, a broken/missing ALSA device, a locked audio server) the
//! engine stores `sink: None` and every [`AudioEngine::play`] call becomes a
//! silent no-op. Audio availability must never block launching the game.
//!
//! The four [`synth_*`] functions are pure and deterministic, which keeps the
//! default sounds testable without a device and lets S05 reuse them as the
//! per-file fallback when a texture/sound pack does not provide an audio file.

use rodio::buffer::SamplesBuffer;
use rodio::{ChannelCount, DeviceSinkError, MixerDeviceSink, SampleRate, Source};

/// Sample rate shared by every synthesized buffer.
pub const SAMPLE_RATE: u32 = 44_100;
/// Channel count of the synthesized buffers (interleaved stereo f32).
pub const CHANNELS: u16 = 2;

/// The game's sound slots. Names are stable because S05's pack override maps
/// file names onto them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SoundSlot {
    Break,
    Place,
    MineComplete,
    UiClick,
}

impl SoundSlot {
    /// Every slot, in buffer order.
    pub const ALL: [SoundSlot; 4] = [
        SoundSlot::Break,
        SoundSlot::Place,
        SoundSlot::MineComplete,
        SoundSlot::UiClick,
    ];

    /// Index into [`AudioEngine::buffers`].
    pub const fn index(self) -> usize {
        match self {
            SoundSlot::Break => 0,
            SoundSlot::Place => 1,
            SoundSlot::MineComplete => 2,
            SoundSlot::UiClick => 3,
        }
    }

    /// Stable pack-facing name (S05 overrides use these file stems).
    pub const fn name(self) -> &'static str {
        match self {
            SoundSlot::Break => "break",
            SoundSlot::Place => "place",
            SoundSlot::MineComplete => "mine_complete",
            SoundSlot::UiClick => "ui_click",
        }
    }
}

// ---------------------------------------------------------------------------
// Deterministic synthesis
// ---------------------------------------------------------------------------

/// Small xorshift64 generator: avoids a `rand` dependency and guarantees the
/// same noise sequence on every call/platform.
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform sample in `[-1.0, 1.0)`.
    fn bipolar(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32 / (1u64 << 23) as f32) * 2.0 - 1.0
    }
}

fn frames(seconds: f32) -> usize {
    (seconds * SAMPLE_RATE as f32).round().max(1.0) as usize
}

fn push_stereo(out: &mut Vec<f32>, sample: f32) {
    let sample = sample.clamp(-1.0, 1.0);
    out.push(sample);
    out.push(sample);
}

/// Block-shatter crunch: a short noise burst with exponential decay and a low
/// body thump. ~0.15 s.
pub fn synth_break() -> Vec<f32> {
    let mut out = Vec::with_capacity(frames(0.15) * CHANNELS as usize);
    let mut rng = XorShift64::new(0x9E37_79B9_7F4A_7C15);
    for i in 0..frames(0.15) {
        let t = i as f32 / SAMPLE_RATE as f32;
        let noise_env = (-t / 0.035).exp();
        let body_env = (-t / 0.045).exp();
        let body = (std::f32::consts::TAU * 92.0 * t).sin() * 0.32;
        let sample = rng.bipolar() * 0.62 * noise_env + body * body_env;
        push_stereo(&mut out, sample);
    }
    out
}

/// Low placement thump with a falling pitch. ~0.08 s.
pub fn synth_place() -> Vec<f32> {
    let len = frames(0.08);
    let mut out = Vec::with_capacity(len * CHANNELS as usize);
    let mut phase = 0.0f32;
    for i in 0..len {
        let t = i as f32 / SAMPLE_RATE as f32;
        // Pitch glides 150 Hz -> 60 Hz.
        let freq = 60.0 + 90.0 * (-t / 0.03).exp();
        phase += std::f32::consts::TAU * freq / SAMPLE_RATE as f32;
        let env = (-t / 0.022).exp();
        let attack = (t / 0.002).min(1.0);
        let sample = phase.sin() * 0.85 * env * attack;
        push_stereo(&mut out, sample);
    }
    out
}

/// Rising "item collected" pop. ~0.08 s.
pub fn synth_mine_complete() -> Vec<f32> {
    let len = frames(0.08);
    let mut out = Vec::with_capacity(len * CHANNELS as usize);
    let mut phase = 0.0f32;
    for i in 0..len {
        let t = i as f32 / SAMPLE_RATE as f32;
        let progress = t / 0.08;
        let freq = 520.0 + 520.0 * progress;
        phase += std::f32::consts::TAU * freq / SAMPLE_RATE as f32;
        let attack = (t / 0.004).min(1.0);
        let env = attack * (-t / 0.035).exp();
        let sample = phase.sin() * 0.55 * env;
        push_stereo(&mut out, sample);
    }
    out
}

/// Short UI tick. ~0.03 s.
pub fn synth_ui_click() -> Vec<f32> {
    let len = frames(0.03);
    let mut out = Vec::with_capacity(len * CHANNELS as usize);
    let mut rng = XorShift64::new(0xD1B5_4A32_D192_ED03);
    for i in 0..len {
        let t = i as f32 / SAMPLE_RATE as f32;
        let env = (-t / 0.0045).exp();
        let tone = (std::f32::consts::TAU * 1_760.0 * t).sin() * 0.5;
        let tick = rng.bipolar() * 0.12;
        push_stereo(&mut out, (tone + tick) * env);
    }
    out
}

/// One decoded sound, interleaved as f32 with its own channel count and sample
/// rate.
///
/// Pack sounds (S05) can be mono 22 050 Hz OGG or stereo 48 000 Hz WAV, so the
/// engine carries the metadata per buffer instead of assuming the synthesized
/// stereo 44 100 Hz shape. rodio's `Mixer::add` wraps every source in
/// `UniformSourceIterator`, which converts rate and channels; the engine never
/// resamples itself.
///
/// Deliberately no `Eq` derive: `f32` has no `Eq`, so this type must never end
/// up inside `LoadedPack` (which derives `Eq`).
#[derive(Clone, Debug, PartialEq)]
pub struct SoundBuffer {
    samples: Vec<f32>,
    channels: u16,
    sample_rate: u32,
}

impl SoundBuffer {
    /// Builds a buffer from interleaved f32 samples and their format.
    pub fn new(samples: Vec<f32>, channels: u16, sample_rate: u32) -> Self {
        Self {
            samples,
            channels,
            sample_rate,
        }
    }

    /// Interleaved f32 samples.
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Channel count of `samples` (1 = mono, 2 = stereo, ...).
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Sample rate of `samples` in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

/// The four built-in fallback buffers, indexed by [`SoundSlot::index`].
pub fn default_buffers() -> [SoundBuffer; 4] {
    [
        SoundBuffer::new(synth_break(), CHANNELS, SAMPLE_RATE),
        SoundBuffer::new(synth_place(), CHANNELS, SAMPLE_RATE),
        SoundBuffer::new(synth_mine_complete(), CHANNELS, SAMPLE_RATE),
        SoundBuffer::new(synth_ui_click(), CHANNELS, SAMPLE_RATE),
    ]
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Rodio-backed sound engine.
///
/// `sink == None` means "no audio device is available": the game keeps running
/// and all playback is silently skipped. Live volume is per-play (derived from
/// the current settings value), so changing the setting needs no engine state
/// synchronization.
pub struct AudioEngine {
    sink: Option<MixerDeviceSink>,
    buffers: [SoundBuffer; 4],
}

impl AudioEngine {
    /// Opens the default output device. Any construction failure degrades to a
    /// silent engine instead of failing the launch.
    pub fn new() -> Self {
        Self::from_sink_result(rodio::DeviceSinkBuilder::open_default_sink())
    }

    /// Explicit headless/test constructor: identical silent no-op path as a
    /// failed device open.
    pub fn disabled() -> Self {
        Self {
            sink: None,
            buffers: default_buffers(),
        }
    }

    /// Maps the rodio construction result onto the fail-soft engine. Kept
    /// separate so tests can inject `Err(DeviceSinkError::NoDevice)` without a
    /// real device.
    fn from_sink_result(result: Result<MixerDeviceSink, DeviceSinkError>) -> Self {
        let sink = result.ok();
        Self {
            sink,
            buffers: default_buffers(),
        }
    }

    /// True when no output device was opened; playback is a no-op.
    pub fn is_silent(&self) -> bool {
        self.sink.is_none()
    }

    /// Replaces every slot buffer, e.g. with pack overrides or the synthesized
    /// defaults. Works on a disabled engine (`sink: None`): no device is needed
    /// to swap buffers, which keeps headless tests and startup ordering simple.
    pub fn set_buffers(&mut self, buffers: [SoundBuffer; 4]) {
        self.buffers = buffers;
    }

    /// Read accessor for one slot's buffer metadata/samples.
    pub fn buffer(&self, slot: SoundSlot) -> &SoundBuffer {
        &self.buffers[slot.index()]
    }

    /// Plays one sound slot at `volume_percent` (0-100; values above 100 are
    /// clamped). 0 mutes outright.
    pub fn play(&self, slot: SoundSlot, volume_percent: u8) {
        if volume_percent == 0 {
            return;
        }
        let Some(sink) = self.sink.as_ref() else {
            return;
        };
        let buffer = &self.buffers[slot.index()];
        let channels = ChannelCount::new(buffer.channels).expect("buffer channels are nonzero");
        let sample_rate =
            SampleRate::new(buffer.sample_rate).expect("buffer sample rate is nonzero");
        let volume = f32::from(volume_percent.min(100)) / 100.0;
        let source = SamplesBuffer::new(channels, sample_rate, buffer.samples.clone())
            .amplify_normalized(volume);
        sink.mixer().add(source);
    }
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_sane(buffer: &[f32]) {
        assert!(!buffer.is_empty(), "synth buffer must not be empty");
        assert_eq!(
            buffer.len() % CHANNELS as usize,
            0,
            "interleaved stereo buffer must have whole frames"
        );
        for (i, sample) in buffer.iter().enumerate() {
            assert!(sample.is_finite(), "sample {i} is not finite: {sample}");
            assert!(
                (-1.0..=1.0).contains(sample),
                "sample {i} out of range: {sample}"
            );
        }
    }

    #[test]
    fn synth_buffers_are_nonempty_finite_and_in_range() {
        assert_sane(&synth_break());
        assert_sane(&synth_place());
        assert_sane(&synth_mine_complete());
        assert_sane(&synth_ui_click());
    }

    #[test]
    fn synth_output_is_deterministic() {
        assert_eq!(synth_break(), synth_break());
        assert_eq!(synth_place(), synth_place());
        assert_eq!(synth_mine_complete(), synth_mine_complete());
        assert_eq!(synth_ui_click(), synth_ui_click());
    }

    #[test]
    fn synth_durations_match_their_slot_character() {
        let sr = SAMPLE_RATE as f32;
        let stereo = CHANNELS as usize;
        assert!((synth_break().len() / stereo) as f32 / sr > 0.10);
        assert!((synth_place().len() / stereo) as f32 / sr < 0.10);
        assert!((synth_mine_complete().len() / stereo) as f32 / sr < 0.10);
        assert!((synth_ui_click().len() / stereo) as f32 / sr < 0.05);
    }

    #[test]
    fn disabled_engine_is_silent_and_play_does_not_panic() {
        let engine = AudioEngine::disabled();
        assert!(engine.sink.is_none());
        assert!(engine.is_silent());
        for slot in SoundSlot::ALL {
            engine.play(slot, 0);
            engine.play(slot, 100);
            engine.play(slot, 255);
        }
    }

    #[test]
    fn construction_error_maps_to_silent_no_op_engine() {
        let engine = AudioEngine::from_sink_result(Err(DeviceSinkError::NoDevice));
        assert!(engine.sink.is_none());
        assert!(engine.is_silent());
        engine.play(SoundSlot::Break, 100);
    }

    #[test]
    fn slot_buffers_are_pairwise_distinct() {
        let buffers = default_buffers();
        for i in 0..buffers.len() {
            for j in (i + 1)..buffers.len() {
                assert_ne!(
                    buffers[i], buffers[j],
                    "slots {i} and {j} share the same default buffer"
                );
            }
        }
    }

    #[test]
    fn slot_names_are_stable_for_pack_overrides() {
        assert_eq!(SoundSlot::Break.name(), "break");
        assert_eq!(SoundSlot::Place.name(), "place");
        assert_eq!(SoundSlot::MineComplete.name(), "mine_complete");
        assert_eq!(SoundSlot::UiClick.name(), "ui_click");
        for (index, slot) in SoundSlot::ALL.iter().enumerate() {
            assert_eq!(slot.index(), index);
        }
    }

    #[test]
    fn default_buffers_carry_stereo_44100_metadata() {
        let buffers = default_buffers();
        for (index, buffer) in buffers.iter().enumerate() {
            assert_eq!(buffer.channels(), CHANNELS, "slot {index} channels");
            assert_eq!(
                buffer.sample_rate(),
                SAMPLE_RATE,
                "slot {index} sample rate"
            );
            assert!(
                !buffer.samples().is_empty(),
                "slot {index} has no samples"
            );
            assert_eq!(
                buffer.samples().len() % buffer.channels() as usize,
                0,
                "slot {index} has a partial frame"
            );
            for (i, sample) in buffer.samples().iter().enumerate() {
                assert!(
                    sample.is_finite(),
                    "slot {index} sample {i} is not finite: {sample}"
                );
            }
        }
    }

    #[test]
    fn default_buffers_match_the_synth_functions() {
        let buffers = default_buffers();
        assert_eq!(buffers[SoundSlot::Break.index()].samples(), synth_break());
        assert_eq!(buffers[SoundSlot::Place.index()].samples(), synth_place());
        assert_eq!(
            buffers[SoundSlot::MineComplete.index()].samples(),
            synth_mine_complete()
        );
        assert_eq!(
            buffers[SoundSlot::UiClick.index()].samples(),
            synth_ui_click()
        );
    }

    #[test]
    fn set_buffers_swaps_per_slot_metadata_on_a_disabled_engine() {
        let mut engine = AudioEngine::disabled();
        assert!(engine.is_silent());
        let custom = [
            SoundBuffer::new(vec![0.1, -0.1, 0.2], 1, 22_050),
            SoundBuffer::new(vec![0.3, 0.4], 2, 48_000),
            SoundBuffer::new(vec![0.0; 4], 2, SAMPLE_RATE),
            SoundBuffer::new(vec![-0.5], 1, 8_000),
        ];
        engine.set_buffers(custom.clone());
        for slot in SoundSlot::ALL {
            let buffer = engine.buffer(slot);
            assert_eq!(*buffer, custom[slot.index()], "slot {slot:?} mismatch");
        }
        assert_eq!(engine.buffer(SoundSlot::Break).channels(), 1);
        assert_eq!(engine.buffer(SoundSlot::Break).sample_rate(), 22_050);
        assert_eq!(engine.buffer(SoundSlot::Place).sample_rate(), 48_000);
        assert!(engine.is_silent(), "swapping buffers must not open a device");
    }

    #[test]
    fn set_buffers_and_play_never_panics_on_a_disabled_engine() {
        let mut engine = AudioEngine::disabled();
        engine.set_buffers([
            SoundBuffer::new(vec![0.0, 0.0], 1, 22_050),
            SoundBuffer::new(Vec::new(), 1, 44_100),
            SoundBuffer::new(vec![0.0; 3], 2, 48_000),
            SoundBuffer::new(vec![0.25], 1, 8_000),
        ]);
        for slot in SoundSlot::ALL {
            engine.play(slot, 0);
            engine.play(slot, 100);
            engine.play(slot, 255);
        }
        engine.set_buffers(default_buffers());
        for slot in SoundSlot::ALL {
            engine.play(slot, 0);
            engine.play(slot, 100);
        }
    }
}
