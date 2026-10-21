//! Pure, GPU-free game-time clock and sky palette.
//!
//! M005's time layer lives here: a [`GameClock`] that advances only while the
//! game is playing and a keyframed [`sky_palette`] used by the renderer for the
//! day factor, fog color, and clear color. Nothing in this module imports
//! `main.rs`, `render`, or `wgpu`, so S03's sun/moon/stars can consume the same
//! math headlessly.

use std::time::Instant;

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

/// (tick, palette) pairs covering 0..=DAY_TICKS; dawn repeats at the end so the
/// wrap at tick 0 is continuous.
const KEYFRAMES: [(u32, SkyPalette); 5] = [
    (0, DAWN),
    (DEFAULT_TIME_OF_DAY, NOON),
    (12_000, DUSK),
    (18_000, NIGHT),
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
        assert!(VISTA_GOLDEN_HOUR > 0 && VISTA_GOLDEN_HOUR < DEFAULT_TIME_OF_DAY);
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
}
