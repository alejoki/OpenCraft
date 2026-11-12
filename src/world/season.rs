//! Pure, GPU-free season and weather derivation.
//!
//! The world calendar is a pure function of `(day_index, season_length_days,
//! seed)` — no state, no chunk edits, no render imports. S05 (seasonal
//! worldgen), S06 (weather presentation), and S08 (mid-winter round-trip) all
//! consume these derivations, so the same inputs must always yield the same
//! season and weather on every launch.
//!
//! Mirrors the deep-module style of [`crate::world::daycycle`]: pinned named
//! constants plus dense headless tests.

/// Default length of one season, in game days.
///
/// New worlds default to a one-day season so the whole year is visible within
/// four days; S02's New Game row and the `--season-length` CLI seam both expose
/// longer options.
pub const DEFAULT_SEASON_LENGTH_DAYS: u32 = 1;

/// Upper bound for a persisted or CLI-provided season length, in game days.
///
/// The New Game cycler tops out at 8 days; a value beyond a full real-length
/// year (365 days) is corruption rather than a choice, so persistence clamps
/// it back to [`DEFAULT_SEASON_LENGTH_DAYS`].
pub const MAX_SEASON_LENGTH_DAYS: u32 = 365;

/// Seasons in one year, in cycle order.
pub const SEASONS_PER_YEAR: u32 = 4;

/// Calendar order of the seasons; `Winter` wraps back to `Spring`.
pub const SEASON_CYCLE: [Season; SEASONS_PER_YEAR as usize] = [
    Season::Spring,
    Season::Summer,
    Season::Autumn,
    Season::Winter,
];

/// First day of Spring, the season every new calendar starts in.
///
/// Boundary pins (MEM184): day 0 is the first day of Spring, so a fresh world
/// begins here. Deliberately named rather than recomputed at runtime so an
/// accidental change to the day math fails a test instead of silently shifting
/// the calendar.
pub const FIRST_SPRING_DAY: u32 = 0;

/// First day of Summer in a Spring-starting calendar at the default length.
///
/// `FIRST_SPRING_DAY + DEFAULT_SEASON_LENGTH_DAYS` = 0 + 1 = 1.
pub const FIRST_SUMMER_DAY: u32 = FIRST_SPRING_DAY + DEFAULT_SEASON_LENGTH_DAYS;

/// First day of Autumn in a Spring-starting calendar at the default length.
///
/// `FIRST_SUMMER_DAY + DEFAULT_SEASON_LENGTH_DAYS` = 2.
pub const FIRST_AUTUMN_DAY: u32 = FIRST_SUMMER_DAY + DEFAULT_SEASON_LENGTH_DAYS;

/// First day of Winter in a Spring-starting calendar at the default length.
///
/// `FIRST_AUTUMN_DAY + DEFAULT_SEASON_LENGTH_DAYS` = 3.
pub const FIRST_WINTER_DAY: u32 = FIRST_AUTUMN_DAY + DEFAULT_SEASON_LENGTH_DAYS;

/// Days in one full year at [`DEFAULT_SEASON_LENGTH_DAYS`].
///
/// Four seasons × 1 day = 4 days; day [`DAYS_PER_YEAR`] is Spring again.
pub const DAYS_PER_YEAR: u32 = SEASONS_PER_YEAR * DEFAULT_SEASON_LENGTH_DAYS;

/// A calendar season.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Season {
    Spring,
    Summer,
    Autumn,
    Winter,
}

impl Season {
    /// Position in [`SEASON_CYCLE`] (0 = Spring, 3 = Winter).
    pub const fn index(self) -> u32 {
        match self {
            Season::Spring => 0,
            Season::Summer => 1,
            Season::Autumn => 2,
            Season::Winter => 3,
        }
    }

    /// Season at `index` in [`SEASON_CYCLE`], wrapping every four seasons.
    pub const fn from_index(index: u32) -> Self {
        SEASON_CYCLE[(index % SEASONS_PER_YEAR) as usize]
    }

    /// The next season in cycle order, wrapping Winter back to Spring.
    pub const fn next(self) -> Self {
        Self::from_index(self.index() + 1)
    }
}

/// The weather state for a day, derived from season + day + seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weather {
    Clear,
    Rain,
    Snowfall,
}

/// Share of days with precipitation in each season, in percent.
///
/// Winter only ever snows; the warmer seasons only ever rain. Clear days are
/// always the majority within a season, so `Clear` interleaves instead of
/// being a rare exception.
pub const WINTER_SNOW_PERCENT: u32 = 40;
pub const SPRING_RAIN_PERCENT: u32 = 30;
pub const SUMMER_RAIN_PERCENT: u32 = 20;
pub const AUTUMN_RAIN_PERCENT: u32 = 35;

/// Creator-tunable share of precipitation days per season, in percent.
///
/// Defaults read the existing constants so the percentages have one home
/// until a live rules bundle overrides them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WeatherRules {
    /// Percent of winter days with snowfall.
    pub winter_snow_percent: u32,
    /// Percent of spring days with rain.
    pub spring_rain_percent: u32,
    /// Percent of summer days with rain.
    pub summer_rain_percent: u32,
    /// Percent of autumn days with rain.
    pub autumn_rain_percent: u32,
}

impl Default for WeatherRules {
    fn default() -> Self {
        Self {
            winter_snow_percent: WINTER_SNOW_PERCENT,
            spring_rain_percent: SPRING_RAIN_PERCENT,
            summer_rain_percent: SUMMER_RAIN_PERCENT,
            autumn_rain_percent: AUTUMN_RAIN_PERCENT,
        }
    }
}

/// FNV-1a 32-bit hash of `seed` followed by `day_index` (both little-endian).
///
/// The high bits are consumed for weather rolls (`>> 8`), which diffuses day
/// changes far better than the low bits of a byte-wise FNV stream.
fn day_hash(seed: u32, day_index: u32) -> u32 {
    let mut hash: u32 = 0x811C_9DC5;
    for byte in seed
        .to_le_bytes()
        .into_iter()
        .chain(day_index.to_le_bytes())
    {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// The season for `day_index` in a world with the given season length.
///
/// **Every new world starts in Spring on day 0**, so the first days are the
/// mildest and a fresh Survival run gets the best chance to establish itself.
/// The calendar then advances one season every `season_length_days` days,
/// wrapping Winter → Spring. The `seed` parameter is retained in the signature
/// because callers derive the calendar from saved world state; it deliberately
/// does not shift the starting season. Pure: repeated calls with the same
/// inputs always return the same season.
///
/// `season_length_days == 0` is treated as one day instead of panicking, so a
/// corrupt persisted scalar can never crash the world; persistence clamps the
/// field as well.
pub fn season_of(day_index: u32, season_length_days: u32, _seed: u32) -> Season {
    let length = season_length_days.max(1);
    let seasons_elapsed = day_index / length;
    Season::from_index(seasons_elapsed)
}

/// The weather for `day_index` in the given season under default rules.
///
/// Mixes seed and day through the shared FNV-1a stream and maps the high bits
/// onto a per-season precipitation chance: Winter can only yield [`Weather::Snowfall`],
/// Spring/Summer/Autumn can only yield [`Weather::Rain`], and every other day
/// is [`Weather::Clear`]. Deterministic for fixed inputs. This is the default
/// wrapper around [`weather_of_with`], kept so every pre-tuning caller (the
/// menu/vista path and the calendar tests) stays byte-identical.
pub fn weather_of(season: Season, day_index: u32, seed: u32) -> Weather {
    weather_of_with(season, day_index, seed, &WeatherRules::default())
}

/// The weather for `day_index` in the given season under live [`WeatherRules`].
///
/// The roll is exactly [`weather_of`]'s FNV-1a stream and high-bit mapping;
/// only the four percentage comparisons are tunable. Winter can only yield
/// [`Weather::Snowfall`], Spring/Summer/Autumn can only yield [`Weather::Rain`],
/// and every other roll is [`Weather::Clear`]. Deterministic for fixed inputs,
/// so a live reload changes which days are wet but never makes one day flip
/// arbitrarily between two runs with the same rules.
pub fn weather_of_with(season: Season, day_index: u32, seed: u32, rules: &WeatherRules) -> Weather {
    let roll = (day_hash(seed, day_index) >> 8) % 100;
    match season {
        Season::Winter if roll < rules.winter_snow_percent => Weather::Snowfall,
        Season::Spring if roll < rules.spring_rain_percent => Weather::Rain,
        Season::Summer if roll < rules.summer_rain_percent => Weather::Rain,
        Season::Autumn if roll < rules.autumn_rain_percent => Weather::Rain,
        _ => Weather::Clear,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weather_rules_default_matches_constants() {
        let rules = WeatherRules::default();
        assert_eq!(rules.winter_snow_percent, WINTER_SNOW_PERCENT);
        assert_eq!(rules.spring_rain_percent, SPRING_RAIN_PERCENT);
        assert_eq!(rules.summer_rain_percent, SUMMER_RAIN_PERCENT);
        assert_eq!(rules.autumn_rain_percent, AUTUMN_RAIN_PERCENT);
    }

    #[test]
    fn weather_of_with_default_rules_is_identical_to_weather_of() {
        let defaults = WeatherRules::default();
        for season in SEASON_CYCLE {
            for seed in [0u32, 1, 2, 42, 12_345, 0xDEAD_BEEF] {
                for day in [0u32, 1, 3, 17, 1_000, u32::MAX] {
                    assert_eq!(
                        weather_of(season, day, seed),
                        weather_of_with(season, day, seed, &defaults),
                        "default-rules mismatch ({season:?}, seed {seed}, day {day})"
                    );
                }
            }
        }
    }

    #[test]
    fn zero_percent_weather_rules_force_clear_in_every_season() {
        let rules = WeatherRules {
            winter_snow_percent: 0,
            spring_rain_percent: 0,
            summer_rain_percent: 0,
            autumn_rain_percent: 0,
        };
        for season in SEASON_CYCLE {
            for seed in [0u32, 1, 2, 42, 0xDEAD_BEEF] {
                for day in [0u32, 1, 7, 999, u32::MAX] {
                    assert_eq!(
                        weather_of_with(season, day, seed, &rules),
                        Weather::Clear,
                        "zero percent must force clear ({season:?}, seed {seed}, day {day})"
                    );
                }
            }
        }
    }

    #[test]
    fn full_percent_weather_rules_force_precipitation_by_season() {
        let rules = WeatherRules {
            winter_snow_percent: 100,
            spring_rain_percent: 100,
            summer_rain_percent: 100,
            autumn_rain_percent: 100,
        };
        for seed in [0u32, 1, 2, 42, 0xDEAD_BEEF] {
            for day in [0u32, 1, 7, 999, u32::MAX] {
                for season in [Season::Spring, Season::Summer, Season::Autumn] {
                    assert_eq!(
                        weather_of_with(season, day, seed, &rules),
                        Weather::Rain,
                        "{season:?} must rain at 100 percent (seed {seed}, day {day})"
                    );
                }
                assert_eq!(
                    weather_of_with(Season::Winter, day, seed, &rules),
                    Weather::Snowfall,
                    "winter must snow at 100 percent (seed {seed}, day {day})"
                );
            }
        }
    }

    #[test]
    fn weather_of_with_changes_only_the_percentage_threshold() {
        // Seed 1, day 0, Spring rolls 18 (< the default 30) -> Rain.
        assert_eq!(weather_of(Season::Spring, 0, 1), Weather::Rain);
        assert_eq!(
            weather_of_with(Season::Spring, 0, 1, &WeatherRules::default()),
            Weather::Rain
        );

        // Only spring is dried out; the same roll now falls above the
        // threshold and the day turns clear.
        let dry_spring = WeatherRules {
            spring_rain_percent: 0,
            ..WeatherRules::default()
        };
        assert_eq!(
            weather_of_with(Season::Spring, 0, 1, &dry_spring),
            Weather::Clear
        );

        // Untuned seasons keep their default thresholds: seed 42, day 0,
        // Winter rolls 8 (< the default 40) -> Snowfall.
        assert_eq!(
            weather_of_with(Season::Winter, 0, 42, &dry_spring),
            Weather::Snowfall
        );
    }

    /// SPRING_START_SEED is the historical fixture seed: day 0 is now Spring
    /// for every seed, so the boundary constants below are seed-independent.
    const SPRING_START_SEED: u32 = 1;

    fn all_seasons_within_four_days(seed: u32) -> bool {
        let mut seen = [false; 4];
        for day in 0..DAYS_PER_YEAR {
            seen[season_of(day, DEFAULT_SEASON_LENGTH_DAYS, seed).index() as usize] = true;
        }
        seen.iter().all(|&s| s)
    }

    #[test]
    fn default_season_length_is_one_day() {
        assert_eq!(DEFAULT_SEASON_LENGTH_DAYS, 1);
        assert_eq!(DAYS_PER_YEAR, 4);
    }

    #[test]
    fn season_cycle_order_is_spring_summer_autumn_winter() {
        assert_eq!(
            SEASON_CYCLE,
            [
                Season::Spring,
                Season::Summer,
                Season::Autumn,
                Season::Winter
            ]
        );
        assert_eq!(Season::Spring.next(), Season::Summer);
        assert_eq!(Season::Summer.next(), Season::Autumn);
        assert_eq!(Season::Autumn.next(), Season::Winter);
        assert_eq!(Season::Winter.next(), Season::Spring);
        assert_eq!(Season::from_index(5), Season::Summer);
    }

    /// MEM184 boundary pin: the first day of each season at length 1.
    #[test]
    fn boundary_day_consts_are_pinned_for_a_spring_start() {
        assert_eq!(FIRST_SPRING_DAY, 0);
        assert_eq!(FIRST_SUMMER_DAY, 1);
        assert_eq!(FIRST_AUTUMN_DAY, 2);
        assert_eq!(FIRST_WINTER_DAY, 3);
        assert_eq!(DAYS_PER_YEAR, 4);

        assert_eq!(
            season_of(FIRST_SPRING_DAY, 1, SPRING_START_SEED),
            Season::Spring
        );
        assert_eq!(
            season_of(FIRST_SUMMER_DAY, 1, SPRING_START_SEED),
            Season::Summer
        );
        assert_eq!(
            season_of(FIRST_AUTUMN_DAY, 1, SPRING_START_SEED),
            Season::Autumn
        );
        assert_eq!(
            season_of(FIRST_WINTER_DAY, 1, SPRING_START_SEED),
            Season::Winter
        );
        assert_eq!(
            season_of(DAYS_PER_YEAR, 1, SPRING_START_SEED),
            Season::Spring,
            "day 4 wraps the year back to Spring"
        );
    }

    #[test]
    fn every_seed_starts_in_spring_and_cycles_in_order() {
        // Fresh worlds always start mild: day 0 is Spring regardless of seed.
        for seed in [0u32, 1, 2, 3, 42, 12_345, 0xDEAD_BEEF, 0x00C0_FFEE] {
            assert_eq!(
                season_of(0, 1, seed),
                Season::Spring,
                "seed {seed} must start in Spring"
            );
            assert_eq!(season_of(1, 1, seed), Season::Summer);
            assert_eq!(season_of(2, 1, seed), Season::Autumn);
            assert_eq!(season_of(3, 1, seed), Season::Winter);
            assert_eq!(season_of(4, 1, seed), Season::Spring);
        }
    }

    #[test]
    fn seed_does_not_shift_the_calendar() {
        // The seed is carried for call-site symmetry with persisted world
        // state, never as a starting-season selector.
        for day in [0u32, 1, 2, 3, 17, 1_000] {
            let expected = season_of(day, DEFAULT_SEASON_LENGTH_DAYS, 0);
            for seed in [1u32, 42, 0xDEAD_BEEF, 0x00C0_FFEE] {
                assert_eq!(season_of(day, DEFAULT_SEASON_LENGTH_DAYS, seed), expected);
            }
        }
    }

    #[test]
    fn every_seed_visits_all_four_seasons_within_four_days_at_length_one() {
        for seed in [0, 1, 2, 3, 42, 12_345, 0xDEAD_BEEF] {
            assert!(
                all_seasons_within_four_days(seed),
                "seed {seed} never visited all four seasons"
            );
        }
    }

    #[test]
    fn longer_seasons_hold_each_season_until_its_boundary() {
        // Seed 1 starts in Spring; at length 2 each season spans two days.
        assert_eq!(season_of(0, 2, SPRING_START_SEED), Season::Spring);
        assert_eq!(season_of(1, 2, SPRING_START_SEED), Season::Spring);
        assert_eq!(season_of(2, 2, SPRING_START_SEED), Season::Summer);
        assert_eq!(season_of(3, 2, SPRING_START_SEED), Season::Summer);
        assert_eq!(season_of(4, 2, SPRING_START_SEED), Season::Autumn);
        assert_eq!(season_of(5, 2, SPRING_START_SEED), Season::Autumn);
        assert_eq!(season_of(6, 2, SPRING_START_SEED), Season::Winter);
        assert_eq!(season_of(7, 2, SPRING_START_SEED), Season::Winter);
        assert_eq!(season_of(8, 2, SPRING_START_SEED), Season::Spring);
    }

    #[test]
    fn season_derivation_is_deterministic() {
        for seed in [0u32, 1, 42, 0xDEAD_BEEF] {
            for length in [0u32, 1, 2, 8, 365] {
                for day in [0u32, 1, 3, 17, 1_000, u32::MAX] {
                    let first = season_of(day, length, seed);
                    for _ in 0..4 {
                        assert_eq!(season_of(day, length, seed), first);
                    }
                }
            }
        }
    }

    /// Persistence clamps a corrupt zero length, but the pure function must
    /// stay defensive on its own: zero behaves exactly like one.
    #[test]
    fn zero_season_length_is_treated_as_one_and_never_panics() {
        for seed in [0u32, 1, 2, 42, u32::MAX] {
            for day in [0u32, 1, 2, 3, 4, 100, u32::MAX] {
                assert_eq!(
                    season_of(day, 0, seed),
                    season_of(day, 1, seed),
                    "length 0 must behave like length 1 (seed {seed}, day {day})"
                );
            }
        }
    }

    #[test]
    fn extreme_inputs_do_not_panic_or_overflow() {
        let _ = season_of(u32::MAX, u32::MAX, u32::MAX);
        let _ = season_of(u32::MAX, 0, 0);
        let _ = weather_of(Season::Winter, u32::MAX, u32::MAX);
        let _ = weather_of(Season::Summer, 0, 0);
    }

    #[test]
    fn fixed_input_weather_examples() {
        // Hand-derived rolls (FNV-1a over seed+day bytes, >> 8, % 100).
        assert_eq!(weather_of(Season::Winter, 0, 42), Weather::Snowfall); // roll 8
        assert_eq!(weather_of(Season::Winter, 4, 42), Weather::Clear); // roll 96
        assert_eq!(weather_of(Season::Spring, 0, 1), Weather::Rain); // roll 18
        assert_eq!(weather_of(Season::Autumn, 2, 1), Weather::Rain); // roll 32
        assert_eq!(weather_of(Season::Summer, 0, 0), Weather::Clear); // roll 93
        assert_eq!(weather_of(Season::Summer, 5, 1), Weather::Rain); // roll 13
    }

    #[test]
    fn winter_yields_snowfall_or_clear_but_never_rain() {
        for seed in [0u32, 1, 2, 3, 42, 12_345, 0xDEAD_BEEF] {
            for day in 0..64 {
                let weather = weather_of(Season::Winter, day, seed);
                assert_ne!(
                    weather,
                    Weather::Rain,
                    "winter produced rain (seed {seed}, day {day})"
                );
            }
        }
    }

    #[test]
    fn warm_seasons_yield_rain_or_clear_but_never_snow() {
        for season in [Season::Spring, Season::Summer, Season::Autumn] {
            for seed in [0u32, 1, 2, 3, 42, 12_345, 0xDEAD_BEEF] {
                for day in 0..64 {
                    assert_ne!(
                        weather_of(season, day, seed),
                        Weather::Snowfall,
                        "{season:?} produced snow (seed {seed}, day {day})"
                    );
                }
            }
        }
    }

    #[test]
    fn clear_interleaves_with_precipitation_across_a_year() {
        // Seed 1 over a full year at length 1: Clear, Rain, and Snowfall all
        // appear, and Clear is never the only outcome.
        let mut clear = 0;
        let mut rain = 0;
        let mut snow = 0;
        for day in 0..32 {
            match weather_of(season_of(day, 1, SPRING_START_SEED), day, SPRING_START_SEED) {
                Weather::Clear => clear += 1,
                Weather::Rain => rain += 1,
                Weather::Snowfall => snow += 1,
            }
        }
        assert!(clear > 0, "no clear days");
        assert!(rain > 0, "no rain days");
        assert!(snow > 0, "no snowfall days");
    }

    #[test]
    fn weather_derivation_is_deterministic() {
        for seed in [0u32, 1, 42, 0xDEAD_BEEF] {
            for season in SEASON_CYCLE {
                for day in [0u32, 1, 7, 1_000, u32::MAX] {
                    let first = weather_of(season, day, seed);
                    for _ in 0..4 {
                        assert_eq!(weather_of(season, day, seed), first);
                    }
                }
            }
        }
    }

    #[test]
    fn weather_varies_by_day_and_seed() {
        let mut seen = [false; 3];
        for day in 0..16 {
            for seed in [1u32, 2, 42] {
                match weather_of(Season::Spring, day, seed) {
                    Weather::Clear => seen[0] = true,
                    Weather::Rain => seen[1] = true,
                    Weather::Snowfall => seen[2] = true,
                }
            }
        }
        assert!(seen[0], "spring never yielded Clear");
        assert!(seen[1], "spring never yielded Rain");
    }
}
