//! Pure, GPU-free game-time clock and sky palette.
//!
//! M005's time layer lives here: a [`GameClock`] that advances only while the
//! game is playing and a keyframed [`sky_palette`] used by the renderer for the
//! day factor, fog color, and clear color. Nothing in this module imports
//! `main.rs`, `render`, or `wgpu`, so S03's sun/moon/stars can consume the same
//! math headlessly.

use std::time::Instant;

use glam::Vec3;

/// Ticks in one full day/night cycle.
///
/// 24,000 ticks over the 20-real-minute (1,200 s) cycle is 20 ticks per
/// second, matching Minecraft's familiar 0 = dawn / 6,000 = noon /
/// 12,000 = dusk / 18,000 = midnight encoding.
pub const DAY_TICKS: u32 = 24_000;

/// Seconds of real time in one full cycle at `time_scale == 1.0`.
pub const DAY_SECONDS: f32 = 1_200.0;

/// Default time of day: noon.
pub const DEFAULT_TIME_OF_DAY: u32 = 6_000;

/// Dimmest the world gets at midnight, roughly one third of full daylight.
pub const MOONLIT_FLOOR: f32 = 0.34;

/// Number of stars in the fixed-seed sky field.
pub const STAR_COUNT: usize = 600;

/// Angular radius of the sun billboard, in radians (~3.4 degrees).
pub const SUN_ANGULAR_SIZE: f32 = 0.06;

/// Angular radius of the moon billboard, in radians (~3.4 degrees).
pub const MOON_ANGULAR_SIZE: f32 = 0.06;

/// Angular radius of a star billboard, in radians (~1.1 degrees).
pub const STAR_ANGULAR_SIZE: f32 = 0.02;

/// Half-width of the smooth horizon band used to fade a body's brightness.
///
/// The sun/moon quad reaches alpha 0 at `direction.y == -HORIZON_FADE_BAND`
/// and full brightness at `+HORIZON_FADE_BAND`, so the body never pops in or
/// out exactly at the horizon.
const HORIZON_FADE_BAND: f32 = 0.08;

/// Fixed seed for [`generate_star_field`]; no wall-clock or global state.
const STAR_SEED: u32 = 0x5EED_5A17;

/// Pinned time of day for the main-menu vista: a dawn-side golden hour that is
/// warm and clearly not noon-blue. This is the single home for the constant;
/// the vista render arm and S03's sun/moon consume it and never redefine it.
pub const VISTA_GOLDEN_HOUR: u32 = 1_000;

/// The player's game clock.
///
/// Time advances only when [`GameClock::tick`] is called with `playing: true`.
/// The clock owns its own frame marker: it disarms while paused and the first
/// frame after resume contributes zero, so paused wall-clock duration can never
/// leak into game time (no `enter_playing` hook required).
#[derive(Debug, Clone, Copy)]
pub struct GameClock {
    /// Whole ticks since dawn, always in `0..DAY_TICKS`.
    pub time_of_day: u32,
    fractional_ticks: f32,
    frame_marker: Option<Instant>,
}

impl GameClock {
    /// A clock starting at [`DEFAULT_TIME_OF_DAY`] (noon).
    pub fn new() -> Self {
        Self::from_time(DEFAULT_TIME_OF_DAY)
    }

    /// A clock starting at `time_of_day`, clamped into the valid cycle.
    ///
    /// `DAY_TICKS` itself wraps to dawn, so it clamps to the final tick of the
    /// day rather than producing an out-of-range stored value.
    pub fn from_time(time_of_day: u32) -> Self {
        Self {
            time_of_day: time_of_day.min(DAY_TICKS - 1),
            fractional_ticks: 0.0,
            frame_marker: None,
        }
    }

    /// Advance the clock for one frame.
    ///
    /// Only playing frames with an existing marker advance. A non-playing call
    /// clears the marker; the next playing call therefore contributes zero even
    /// if minutes of wall-clock time elapsed while paused.
    pub fn tick(&mut self, playing: bool, now: Instant, time_scale: f32) {
        if playing {
            if let Some(previous) = self.frame_marker {
                let delta_secs = now.saturating_duration_since(previous).as_secs_f32();
                self.advance_secs(delta_secs, time_scale);
            }
            self.frame_marker = Some(now);
        } else {
            // Self-contained disarm: the first playing frame after resume has
            // no marker, so the paused wall-clock duration cannot leak in.
            self.frame_marker = None;
        }
    }

    /// Advance by `delta_secs` of real time at the given acceleration scale.
    ///
    /// Pure math, no `Instant`: one second at scale 1.0 is 20 ticks. Fractional
    /// ticks are accumulated because a 60 fps frame is only ~0.33 ticks —
    /// truncating every frame would stall the clock forever.
    pub fn advance_secs(&mut self, delta_secs: f32, time_scale: f32) {
        if !delta_secs.is_finite() || delta_secs <= 0.0 {
            return;
        }
        if !time_scale.is_finite() || time_scale <= 0.0 {
            return;
        }

        self.fractional_ticks += delta_secs * time_scale * (DAY_TICKS as f32 / DAY_SECONDS);

        // Keep the sub-tick remainder; a 60 fps frame is only ~0.33 ticks, so
        // truncating each frame would freeze the clock.
        let whole_ticks = self.fractional_ticks.trunc();
        self.fractional_ticks -= whole_ticks;
        let advance = (whole_ticks as u64 % DAY_TICKS as u64) as u32;
        self.time_of_day = (self.time_of_day + advance) % DAY_TICKS;
    }
}

impl Default for GameClock {
    fn default() -> Self {
        Self::new()
    }
}

/// Render-facing sky state derived from the time of day.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyPalette {
    /// Multiplier applied to terrain vertex light; 1.0 at noon, dimmer at night.
    pub day_factor: f32,
    /// Linear RGB sky/background color.
    pub sky_color: [f32; 3],
    /// Linear RGB fog color the terrain fades into.
    pub fog_color: [f32; 3],
}

/// Linear keyframe palette over one cycle (dawn → noon → dusk → night → dawn).
pub fn sky_palette(time_of_day: u32) -> SkyPalette {
    let t = time_of_day % DAY_TICKS;

    for window in KEYFRAMES.windows(2) {
        let (start_tick, start) = window[0];
        let (end_tick, end) = window[1];
        if t >= start_tick && t <= end_tick {
            // Return the keyframe exactly at its boundary so the default-time
            // noon colors are bit-identical to today's hardcoded values.
            if t == start_tick {
                return start;
            }
            if t == end_tick {
                return end;
            }
            let span = (end_tick - start_tick) as f32;
            let fraction = (t - start_tick) as f32 / span;
            return SkyPalette {
                day_factor: lerp(start.day_factor, end.day_factor, fraction),
                sky_color: lerp_color(start.sky_color, end.sky_color, fraction),
                fog_color: lerp_color(start.fog_color, end.fog_color, fraction),
            };
        }
    }

    // The keyframes span the whole 0..=DAY_TICKS range; this is unreachable.
    DAWN
}

/// Dawn keyframe: warm, low sun, clearly not noon-blue.
const DAWN: SkyPalette = SkyPalette {
    day_factor: 0.55,
    sky_color: [0.96, 0.66, 0.42],
    fog_color: [0.92, 0.68, 0.48],
};

/// Noon keyframe: today's hardcoded sky/fog blue with full brightness, so
/// default-time daytime rendering is visually unchanged.
const NOON: SkyPalette = SkyPalette {
    day_factor: 1.0,
    sky_color: [0.53, 0.76, 0.96],
    fog_color: [0.53, 0.76, 0.96],
};

/// Dusk keyframe: the mirror of dawn.
const DUSK: SkyPalette = SkyPalette {
    day_factor: DAWN.day_factor,
    sky_color: DAWN.sky_color,
    fog_color: DAWN.fog_color,
};

/// Midnight keyframe: dim moonlit blue at the named floor.
const NIGHT: SkyPalette = SkyPalette {
    day_factor: MOONLIT_FLOOR,
    sky_color: [0.035, 0.05, 0.12],
    fog_color: [0.05, 0.07, 0.15],
};

/// Tick at which the dusk-to-night ramp completes (and the moonlit hold starts).
const NIGHTFALL_TICK: u32 = 13_200;

/// Tick at which the night hold ends and the pre-dawn ramp toward DAWN starts.
const PRE_DAWN_TICK: u32 = 22_800;

/// (tick, palette) pairs covering 0..=DAY_TICKS; dawn repeats at the end so the
/// wrap at tick 0 is continuous.
///
/// The dusk-to-night ramp is deliberately short (12,000..=13,200, 60 s of real
/// time at scale 1) instead of spanning to 18,000: human UAT (UAT-H2) showed
/// the long ramp left muddy warm dusk colors on screen for ~5 real minutes.
/// The pre-dawn ramp is its mirror (22,800..=24,000), and both night keyframes
/// carry the exact NIGHT palette so the night hold is bit-identical.
const KEYFRAMES: [(u32, SkyPalette); 6] = [
    (0, DAWN),
    (DEFAULT_TIME_OF_DAY, NOON),
    (12_000, DUSK),
    (NIGHTFALL_TICK, NIGHT),
    (PRE_DAWN_TICK, NIGHT),
    (DAY_TICKS, DAWN),
];

fn lerp(start: f32, end: f32, fraction: f32) -> f32 {
    start + (end - start) * fraction
}

fn lerp_color(start: [f32; 3], end: [f32; 3], fraction: f32) -> [f32; 3] {
    [
        lerp(start[0], end[0], fraction),
        lerp(start[1], end[1], fraction),
        lerp(start[2], end[2], fraction),
    ]
}

/// Angle of a celestial body's orbit for a time of day, in radians.
fn body_angle(time_of_day: u32) -> f32 {
    let t = time_of_day % DAY_TICKS;
    std::f32::consts::TAU * t as f32 / DAY_TICKS as f32
}

/// Direction of the sun on the sky unit sphere.
///
/// `0` is dawn on the eastern horizon `(1, 0, 0)`, `6,000` is noon at the
/// zenith `(0, 1, 0)`, `12,000` is dusk on the western horizon `(-1, 0, 0)`,
/// and `18,000` is midnight below the horizon. The orbit lies in the XY plane
/// with `+Y` up, matching the renderer's world axes.
pub fn sun_direction(time_of_day: u32) -> Vec3 {
    let theta = body_angle(time_of_day);
    Vec3::new(theta.cos(), theta.sin(), 0.0)
}

/// Direction of the moon: exactly opposite the sun (high at midnight).
pub fn moon_direction(time_of_day: u32) -> Vec3 {
    -sun_direction(time_of_day)
}

/// Smooth 0..1 fade for a body height, saturating outside the horizon band.
fn horizon_fade(height: f32) -> f32 {
    let x = ((height + HORIZON_FADE_BAND) / (2.0 * HORIZON_FADE_BAND)).clamp(0.0, 1.0);
    // Smoothstep so the brightness ramp has no visible kink at the band edges.
    x * x * (3.0 - 2.0 * x)
}

/// Sun billboard brightness in `0..=1`; `0` once the sun is below the horizon.
pub fn sun_brightness(time_of_day: u32) -> f32 {
    horizon_fade(sun_direction(time_of_day).y)
}

/// Moon billboard brightness in `0..=1`; `0` once the moon is below the horizon.
pub fn moon_brightness(time_of_day: u32) -> f32 {
    horizon_fade(moon_direction(time_of_day).y)
}

/// Star-field visibility in `0..=1`.
///
/// Zero through the day, ramps up from dusk (12,000) to midnight (18,000),
/// then ramps back down to dawn (24,000). The window matches the palette
/// keyframes so stars appear exactly as the sky darkens.
pub fn star_visibility(time_of_day: u32) -> f32 {
    let t = time_of_day % DAY_TICKS;
    const DUSK_TICK: u32 = 12_000;
    const MIDNIGHT_TICK: u32 = 18_000;
    if t <= DUSK_TICK {
        0.0
    } else if t < MIDNIGHT_TICK {
        let fraction = (t - DUSK_TICK) as f32 / (MIDNIGHT_TICK - DUSK_TICK) as f32;
        smoothstep(fraction)
    } else {
        let fraction = (t - MIDNIGHT_TICK) as f32 / (DAY_TICKS - MIDNIGHT_TICK) as f32;
        smoothstep(1.0 - fraction)
    }
}

fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Deterministic fixed-seed star field of [`STAR_COUNT`] unit directions.
///
/// Directions are spread over the whole sphere (roughly half above the
/// horizon). The field is generated once per renderer instance from a private
/// xorshift32 stream, so two calls are byte-identical and nothing depends on
/// the wall clock.
pub fn generate_star_field() -> Vec<Vec3> {
    let mut rng = Xorshift32::new(STAR_SEED);
    let mut stars = Vec::with_capacity(STAR_COUNT);
    for _ in 0..STAR_COUNT {
        let u = (rng.next() >> 8) as f32 / (1u32 << 24) as f32;
        let v = (rng.next() >> 8) as f32 / (1u32 << 24) as f32;
        // Uniform sphere: z is uniform in -1..1, phi uniform in 0..TAU.
        let y = 2.0 * u - 1.0;
        let phi = std::f32::consts::TAU * v;
        let radius = (1.0 - y * y).max(0.0).sqrt();
        stars.push(Vec3::new(radius * phi.cos(), y, radius * phi.sin()));
    }
    stars
}

/// Deterministic per-field xorshift32 stream. No wall-clock or global state,
/// so the star field is identical on every launch.
struct Xorshift32(u32);

impl Xorshift32 {
    #[inline]
    fn new(seed: u32) -> Self {
        // xorshift32 locks up at zero, so force a non-zero state.
        Self(seed | 1)
    }

    #[inline]
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const NOON_SKY: [f32; 3] = [0.53, 0.76, 0.96];

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "expected {expected}, got {actual}"
        );
    }

    fn assert_color_close(actual: [f32; 3], expected: [f32; 3]) {
        for (a, e) in actual.iter().zip(expected.iter()) {
            assert!((a - e).abs() < 1e-6, "expected {expected:?}, got {actual:?}");
        }
    }

    #[test]
    fn cycle_constants_match_the_minecraft_familiar_encoding() {
        assert_eq!(DAY_TICKS, 24_000);
        assert_eq!(DEFAULT_TIME_OF_DAY, 6_000);
        assert_eq!(DAY_TICKS as f32 / DAY_SECONDS, 20.0);
        assert!(
            (0.3..0.4).contains(&MOONLIT_FLOOR),
            "moonlit floor must be roughly one third, got {MOONLIT_FLOOR}"
        );
        const {
            assert!(VISTA_GOLDEN_HOUR > 0 && VISTA_GOLDEN_HOUR < DEFAULT_TIME_OF_DAY)
        };
    }

    #[test]
    fn new_clock_starts_at_default_noon_with_no_fraction() {
        let clock = GameClock::new();
        assert_eq!(clock.time_of_day, DEFAULT_TIME_OF_DAY);
        assert_eq!(clock.fractional_ticks, 0.0);
        assert!(clock.frame_marker.is_none());
    }

    #[test]
    fn from_time_clamps_out_of_range_input() {
        assert_eq!(GameClock::from_time(0).time_of_day, 0);
        assert_eq!(GameClock::from_time(12_345).time_of_day, 12_345);
        assert_eq!(
            GameClock::from_time(DAY_TICKS).time_of_day,
            DAY_TICKS - 1
        );
        assert_eq!(
            GameClock::from_time(u32::MAX).time_of_day,
            DAY_TICKS - 1
        );
    }

    #[test]
    fn advance_secs_at_scale_one_moves_twenty_ticks_per_second() {
        let mut clock = GameClock::from_time(1_000);
        clock.advance_secs(1.0, 1.0);
        assert_eq!(clock.time_of_day, 1_020);
        clock.advance_secs(0.5, 1.0);
        assert_eq!(clock.time_of_day, 1_030);
    }

    #[test]
    fn advance_secs_keeps_fractional_ticks_between_frames() {
        let mut clock = GameClock::from_time(6_000);
        let frame = 1.0 / 60.0;
        for _ in 0..60 {
            clock.advance_secs(frame, 1.0);
        }
        assert_eq!(clock.time_of_day, 6_020);
        assert!(clock.fractional_ticks < 1.0);
    }

    #[test]
    fn advance_secs_wraps_at_the_end_of_the_cycle() {
        let mut clock = GameClock::from_time(DAY_TICKS - 10);
        clock.advance_secs(1.0, 1.0);
        assert_eq!(clock.time_of_day, 10);
    }

    #[test]
    fn scale_sixty_covers_a_full_cycle_in_twenty_seconds() {
        let mut clock = GameClock::from_time(6_000);
        clock.advance_secs(20.0, 60.0);
        assert_eq!(clock.time_of_day, 6_000);
        assert!(clock.fractional_ticks < 1.0);
    }

    #[test]
    fn advance_secs_ignores_non_positive_and_non_finite_input() {
        let mut clock = GameClock::from_time(6_000);

        clock.advance_secs(0.0, 1.0);
        clock.advance_secs(-5.0, 1.0);
        clock.advance_secs(f32::NAN, 1.0);
        clock.advance_secs(f32::INFINITY, 1.0);
        clock.advance_secs(1.0, 0.0);
        clock.advance_secs(1.0, -1.0);
        clock.advance_secs(1.0, f32::NAN);
        clock.advance_secs(1.0, f32::INFINITY);

        assert_eq!(clock.time_of_day, 6_000);
        assert_eq!(clock.fractional_ticks, 0.0);
    }

    #[test]
    fn tick_advances_only_while_playing() {
        let a = Instant::now();
        let mut clock = GameClock::from_time(6_000);

        // Arming frame: no prior marker, so it contributes nothing.
        clock.tick(true, a, 1.0);
        assert_eq!(clock.time_of_day, 6_000);

        clock.tick(true, a + Duration::from_secs(1), 1.0);
        assert_eq!(clock.time_of_day, 6_020);

        clock.tick(false, a + Duration::from_secs(2), 1.0);
        assert!(clock.frame_marker.is_none());
        assert_eq!(clock.time_of_day, 6_020);
    }

    #[test]
    fn pause_then_resume_does_not_leak_paused_duration() {
        let a = Instant::now();
        let mut clock = GameClock::from_time(6_000);

        clock.tick(true, a, 1.0);
        clock.tick(true, a + Duration::from_millis(16), 1.0);
        let paused_time = clock.time_of_day;
        let paused_fraction = clock.fractional_ticks;

        // Ten minutes of wall-clock pause.
        clock.tick(false, a + Duration::from_millis(32), 1.0);
        clock.tick(true, a + Duration::from_secs(600), 1.0);

        assert_eq!(clock.time_of_day, paused_time);
        assert_eq!(clock.fractional_ticks, paused_fraction);

        // The next real frame advances only one normal frame's worth.
        clock.tick(true, a + Duration::from_secs(600) + Duration::from_millis(16), 1.0);
        let advanced = clock.fractional_ticks - paused_fraction
            + (clock.time_of_day - paused_time) as f32;
        assert!(advanced > 0.0);
        assert!(advanced < 1.0, "advanced {advanced} ticks across resume");
    }

    #[test]
    fn tick_wraps_at_the_end_of_the_cycle() {
        let a = Instant::now();
        let mut clock = GameClock::from_time(DAY_TICKS - 5);
        clock.tick(true, a, 1.0);
        clock.tick(true, a + Duration::from_secs(1), 1.0);
        assert_eq!(clock.time_of_day, 15);
    }

    #[test]
    fn default_time_is_exact_noon_blue_with_full_day_factor() {
        let palette = sky_palette(DEFAULT_TIME_OF_DAY);
        assert_eq!(palette.day_factor, 1.0);
        assert_eq!(palette.sky_color, NOON_SKY);
        assert_eq!(palette.fog_color, NOON_SKY);
    }

    #[test]
    fn dusk_is_exact_dusk_keyframe() {
        assert_eq!(sky_palette(12_000), DUSK);
    }

    #[test]
    fn dawn_is_exact_dawn_keyframe() {
        assert_eq!(sky_palette(0), DAWN);
        assert_eq!(sky_palette(DAY_TICKS), DAWN);
    }

    #[test]
    fn midnight_is_exact_night_keyframe() {
        // The "fails if the midnight values are not the night keyframe"
        // clause: equality against the keyframe, not epsilon closeness.
        assert_eq!(sky_palette(18_000), NIGHT);
        assert_eq!(sky_palette(NIGHTFALL_TICK), NIGHT);
        assert_eq!(sky_palette(PRE_DAWN_TICK), NIGHT);
    }

    #[test]
    fn nightfall_completes_within_twelve_hundred_ticks() {
        // UAT-H2: the old ramp DUSK(12,000) -> NIGHT(18,000) painted muddy
        // warm dusk for ~5 real minutes. The ramp must finish at 13,200 ticks
        // (60 s real, 3 s at --time-scale 20) and then hold.
        assert_eq!(NIGHTFALL_TICK - 12_000, 1_200);
        assert_eq!(DAY_TICKS - PRE_DAWN_TICK, 1_200);
        // Symmetric pre-dawn ramp: 22,800 -> 24,000 mirrors 12,000 -> 13,200.
        for offset in [0u32, 300, 600, 900, 1_200] {
            let dusk = sky_palette(12_000 + offset);
            let pre_dawn = sky_palette((DAY_TICKS - offset) % DAY_TICKS);
            assert_close(dusk.day_factor, pre_dawn.day_factor);
            assert_color_close(dusk.sky_color, pre_dawn.sky_color);
            assert_color_close(dusk.fog_color, pre_dawn.fog_color);
        }
    }

    #[test]
    fn sky_is_decisively_night_shortly_after_dusk() {
        // Tick 14,000 is past the completed 1,200-tick ramp: the sky must be
        // decisively blue-dominant, not a muddy dusk lerp.
        let palette = sky_palette(14_000);
        assert_eq!(palette, NIGHT);
        let [r, g, b] = palette.sky_color;
        assert!(
            b > r + 0.05,
            "night sky should be clearly blue-dominant, got {palette:?}"
        );
        assert!(
            b > g,
            "night blue channel should beat green, got {palette:?}"
        );
        assert_close(palette.day_factor, MOONLIT_FLOOR);
    }

    #[test]
    fn midnight_sits_on_the_moonlit_floor() {
        assert_close(sky_palette(18_000).day_factor, MOONLIT_FLOOR);
    }

    #[test]
    fn day_factor_is_monotonic_from_noon_through_midnight_to_dawn() {
        let mut previous = sky_palette(DEFAULT_TIME_OF_DAY).day_factor;
        for t in (DEFAULT_TIME_OF_DAY..=18_000).step_by(250) {
            let factor = sky_palette(t).day_factor;
            assert!(
                factor <= previous + 1e-6,
                "day factor rose from {previous} to {factor} at tick {t}"
            );
            previous = factor;
        }
        previous = sky_palette(18_000).day_factor;
        for t in (18_000..=DAY_TICKS).step_by(250) {
            let factor = sky_palette(t % DAY_TICKS).day_factor;
            assert!(
                factor >= previous - 1e-6,
                "day factor fell from {previous} to {factor} at tick {t}"
            );
            previous = factor;
        }
    }

    #[test]
    fn dawn_and_dusk_are_mirror_images() {
        for offset in [0u32, 1_000, 3_000, 6_000] {
            let dusk = sky_palette(12_000 + offset);
            let dawn = sky_palette((DAY_TICKS - offset) % DAY_TICKS);
            assert_close(dusk.day_factor, dawn.day_factor);
            assert_color_close(dusk.sky_color, dawn.sky_color);
            assert_color_close(dusk.fog_color, dawn.fog_color);

            let before_noon = sky_palette(DEFAULT_TIME_OF_DAY - offset);
            let after_noon = sky_palette(DEFAULT_TIME_OF_DAY + offset);
            assert_close(before_noon.day_factor, after_noon.day_factor);
            assert_color_close(before_noon.sky_color, after_noon.sky_color);
            assert_color_close(before_noon.fog_color, after_noon.fog_color);
        }
    }

    #[test]
    fn fog_tracks_sky_closely() {
        for t in (0..DAY_TICKS).step_by(500) {
            let palette = sky_palette(t);
            for channel in 0..3 {
                let delta = (palette.fog_color[channel] - palette.sky_color[channel]).abs();
                assert!(
                    delta < 0.15,
                    "fog drifted from sky at tick {t}: {palette:?}"
                );
            }
        }
    }

    #[test]
    fn vista_golden_hour_is_warm_and_not_noon_blue() {
        let palette = sky_palette(VISTA_GOLDEN_HOUR);
        assert!(palette.day_factor < 1.0);
        assert!(
            palette.sky_color[0] > palette.sky_color[2],
            "golden hour should be warm, got {:?}",
            palette.sky_color
        );
        assert_ne!(palette.sky_color, NOON_SKY);
    }

    #[test]
    fn sky_palette_wraps_out_of_range_time() {
        assert_eq!(sky_palette(DAY_TICKS + DEFAULT_TIME_OF_DAY), sky_palette(DEFAULT_TIME_OF_DAY));
        assert_eq!(sky_palette(u32::MAX), sky_palette(u32::MAX % DAY_TICKS));
    }

    fn assert_vec_close(actual: Vec3, expected: Vec3) {
        for (a, e) in [
            (actual.x, expected.x),
            (actual.y, expected.y),
            (actual.z, expected.z),
        ] {
            assert!((a - e).abs() < 1e-5, "expected {expected}, got {actual}");
        }
    }

    #[test]
    fn sun_direction_hits_dawn_noon_dusk_and_midnight() {
        assert_vec_close(sun_direction(0), Vec3::new(1.0, 0.0, 0.0));
        assert_vec_close(sun_direction(6_000), Vec3::new(0.0, 1.0, 0.0));
        assert_vec_close(sun_direction(12_000), Vec3::new(-1.0, 0.0, 0.0));
        assert_vec_close(sun_direction(18_000), Vec3::new(0.0, -1.0, 0.0));
    }

    #[test]
    fn sun_direction_is_a_unit_vector_across_the_cycle() {
        for t in (0..DAY_TICKS).step_by(500) {
            let sun = sun_direction(t);
            assert!((sun.length() - 1.0).abs() < 1e-5, "sun at {t}: {sun}");
            assert!(sun.is_finite());
            assert_eq!(sun.z, 0.0, "the sun orbit stays in the XY plane");
        }
    }

    #[test]
    fn moon_is_exactly_opposite_the_sun() {
        for t in (0..DAY_TICKS).step_by(750) {
            let sun = sun_direction(t);
            let moon = moon_direction(t);
            assert_eq!(moon, -sun, "moon must be antipodal at tick {t}");
            assert!((moon.length() - 1.0).abs() < 1e-5, "moon at {t}: {moon}");
        }
    }

    #[test]
    fn moon_is_high_at_midnight_and_below_the_horizon_at_noon() {
        assert_vec_close(moon_direction(18_000), Vec3::new(0.0, 1.0, 0.0));
        assert!(moon_direction(6_000).y < 0.0);
        assert!(moon_brightness(18_000) > 0.99);
        assert_eq!(moon_brightness(6_000), 0.0);
    }

    #[test]
    fn sun_brightness_is_zero_below_horizon_and_full_at_transit() {
        assert_eq!(sun_brightness(18_000), 0.0);
        assert_close(sun_brightness(0), 0.5);
        assert!(sun_brightness(6_000) > 0.99);
        // The horizon fade lifts the sun smoothly through dawn.
        assert!(sun_brightness(50) < sun_brightness(150));
        assert!(sun_brightness(150) < sun_brightness(300));
        for t in (0..DAY_TICKS).step_by(250) {
            let brightness = sun_brightness(t);
            assert!((0.0..=1.0).contains(&brightness), "sun at {t}: {brightness}");
        }
    }

    #[test]
    fn moon_brightness_is_the_opposite_phase() {
        for t in (0..DAY_TICKS).step_by(500) {
            assert_close(
                moon_brightness(t),
                sun_brightness((t + 12_000) % DAY_TICKS),
            );
        }
        assert!(moon_brightness(18_000) > 0.99);
        assert_eq!(moon_brightness(6_000), 0.0);
    }

    #[test]
    fn star_visibility_is_zero_by_day_and_peaks_at_midnight() {
        for t in [0u32, 2_000, 6_000, 10_000, 12_000] {
            assert_eq!(star_visibility(t), 0.0, "stars visible at tick {t}");
        }
        assert!(star_visibility(18_000) > 0.99);
        assert!(star_visibility(15_000) > 0.0 && star_visibility(15_000) < 1.0);
        assert!(star_visibility(21_000) > 0.0 && star_visibility(21_000) < 1.0);
        assert_eq!(star_visibility(DAY_TICKS), 0.0);
    }

    #[test]
    fn star_visibility_ramps_monotonically_in_and_out_of_the_night() {
        let mut previous = star_visibility(12_000);
        for t in (12_000..=18_000).step_by(250) {
            let value = star_visibility(t);
            assert!(value >= previous - 1e-6, "dusk ramp fell at {t}");
            previous = value;
        }
        previous = star_visibility(18_000);
        for t in (18_000..=DAY_TICKS).step_by(250) {
            let value = star_visibility(t % DAY_TICKS);
            assert!(value <= previous + 1e-6, "dawn ramp rose at {t}");
            previous = value;
        }
        assert_eq!(star_visibility(DAY_TICKS), 0.0);
    }

    #[test]
    fn star_visibility_wraps_out_of_range_time() {
        for t in [DAY_TICKS + 15_000, DAY_TICKS + 21_000, u32::MAX] {
            assert_eq!(star_visibility(t), star_visibility(t % DAY_TICKS));
        }
    }

    #[test]
    fn generate_star_field_is_deterministic() {
        let first = generate_star_field();
        let second = generate_star_field();
        assert_eq!(first.len(), second.len());
        for (a, b) in first.iter().zip(second.iter()) {
            assert_eq!(a.x.to_bits(), b.x.to_bits());
            assert_eq!(a.y.to_bits(), b.y.to_bits());
            assert_eq!(a.z.to_bits(), b.z.to_bits());
        }
    }

    #[test]
    fn star_field_has_expected_count_and_unit_directions() {
        let stars = generate_star_field();
        assert_eq!(stars.len(), STAR_COUNT);
        for star in &stars {
            assert!(star.is_finite(), "non-finite star {star}");
            assert!(
                (star.length() - 1.0).abs() < 1e-5,
                "star is not unit length: {star}"
            );
        }
    }

    #[test]
    fn roughly_half_the_star_field_sits_above_the_horizon() {
        let stars = generate_star_field();
        let above = stars.iter().filter(|star| star.y > 0.0).count();
        let fraction = above as f32 / stars.len() as f32;
        assert!(
            (0.4..=0.6).contains(&fraction),
            "only {above}/{} stars above the horizon",
            stars.len()
        );
    }

    #[test]
    fn angular_size_constants_stay_in_readable_range() {
        assert!((0.05..=0.08).contains(&SUN_ANGULAR_SIZE));
        assert!((0.05..=0.08).contains(&MOON_ANGULAR_SIZE));
        assert!((0.01..=0.03).contains(&STAR_ANGULAR_SIZE));
        const {
            assert!(STAR_ANGULAR_SIZE < SUN_ANGULAR_SIZE);
            assert!(SUN_ANGULAR_SIZE < std::f32::consts::FRAC_PI_4);
        };
    }
}
