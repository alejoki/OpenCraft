//! Headless demo seams and their bounded watch/sampler support; every item
//! here is a pure move out of the bin crate (S09) and is exercised by the real
//! binary through main()'s dispatch.

use std::time::{Duration, Instant};

use crate::world::daycycle;
use crate::world::persistence::{CliConfig, DEFAULT_TIME_SCALE};

/// Accelerated-watch suppression predicate: true when the launch requested a
/// scripted demo seam (`--demo-swim`, `--demo-survival`, `--demo-content`, or
/// `--demo-combat`).
/// A scripted demo owns its own completion and digest contract at its own
/// pace, so the accelerated `--time-scale` watch and its wall-clock deadline
/// must stay disarmed even when `--time-scale` is also on the command line —
/// otherwise a `--demo-content --time-scale 60` launch could shut down before
/// the scripted run prints its own digest. The flags themselves are still
/// launch switches, never world intents (see the bin crate's
/// `cli_has_world_intent`).
pub fn scripted_demo_requested(cli: &CliConfig) -> bool {
    cli.demo_swim || cli.demo_survival || cli.demo_content || cli.demo_combat
}

/// Whether this launch is a bounded demo/watch seam that owns its own world
/// witness: the four scripted demos plus the accelerated multi-cycle watch.
///
/// The watch arms on exactly [`demo_shutdown_deadline`]'s condition
/// (`time_scale > DEFAULT_TIME_SCALE`), never on `--demo-days` alone — that
/// flag defaults to 1, so treating it as the arming signal would close the gate
/// for every production launch.
///
/// Those runs pin exact save layouts — several real-binary scripts assert a
/// zero v8 mob count on an "edit-free fresh world" — so the ambient population
/// layer stays closed for their whole life. That is the same ephemeral demo
/// switch `--demo-combat` uses around its own pinned stages (M009/S05-T03):
/// creation and daylight burning are off, the distance cull still runs.
/// Production launches never touch it, so the shipped game keeps spawning.
pub fn demo_owns_world_witness(cli: &CliConfig) -> bool {
    scripted_demo_requested(cli) || cli.time_scale > DEFAULT_TIME_SCALE
}

/// Bounded accelerated-demo watch (S02-T04, generalized to N cycles in
/// S06-T05): `--time-scale` is a test seam for observing whole day cycles.
/// Sums the game-time ticks actually advanced by the frame loop and reports
/// completion once `demo_days` whole `DAY_TICKS` cycles have elapsed.
/// Frame-rate independent because it reads the clock, not the wall time.
///
/// The budget is a single summed tick counter rather than a per-cycle latch:
/// the watch completes exactly when `DAY_TICKS * days` ticks have been
/// observed, so a `days = 4` demo cannot finish early on an integer number of
/// wrapped cycles and a `days = 1` demo is byte-identical to the pre-S06
/// behavior that `scripts/season_smoke.sh` asserts.
#[derive(Debug)]
pub struct DemoCycleWatch {
    remaining_ticks: u32,
    last_time_of_day: u32,
}

impl DemoCycleWatch {
    pub fn new(time_of_day: u32, days: u32) -> Self {
        Self {
            // `days` is already clamped by `CliConfig`, but `.max(1)` keeps a
            // zero from arming a watch that completes on the first frame.
            remaining_ticks: daycycle::DAY_TICKS.saturating_mul(days.max(1)),
            last_time_of_day: time_of_day % daycycle::DAY_TICKS,
        }
    }

    /// Feed one frame's time of day. Returns true exactly when the summed
    /// advance reaches the configured number of cycles (wrap-safe at every
    /// cycle boundary).
    pub fn observe(&mut self, time_of_day: u32) -> bool {
        let time_of_day = time_of_day % daycycle::DAY_TICKS;
        let delta =
            (time_of_day + daycycle::DAY_TICKS - self.last_time_of_day) % daycycle::DAY_TICKS;
        self.last_time_of_day = time_of_day;
        self.remaining_ticks = self.remaining_ticks.saturating_sub(delta);
        self.remaining_ticks == 0
    }
}

/// Arm/observe the bounded accelerated demo in one place, shared by the
/// RedrawRequested pass and its headless tests. Never arms at the default
/// 1.0 scale (a normal run is unbounded) and never counts paused frames
/// (the clock itself is frozen there). `demo_days` is the app's `--demo-days`
/// value; it only matters when the accelerated watch is armed.
pub fn observe_demo_cycle(
    watch: &mut Option<DemoCycleWatch>,
    time_scale: f32,
    demo_days: u32,
    playing: bool,
    time_of_day: u32,
) -> bool {
    if !playing || time_scale <= DEFAULT_TIME_SCALE {
        return false;
    }
    match watch {
        Some(watch) => watch.observe(time_of_day),
        None => {
            *watch = Some(DemoCycleWatch::new(time_of_day, demo_days));
            false
        }
    }
}

/// Hard cap on retained per-frame samples for the demo-shutdown frame-budget
/// digest (S08-T01). At uncapped frame rates a few minutes of demo is well
/// under this; the cap only guarantees bounded memory if a run is left going.
pub const FRAME_BUDGET_SAMPLE_CAP: usize = 120_000;
/// The operational frame-time target quoted in the digest (MEM229): a bounded
/// release run must hold p95 wall frame time at or under one 60 Hz frame.
pub const FRAME_BUDGET_TARGET_MS: f64 = 16.6;

/// Percentiles over a bounded per-frame duration sample (S08-T01).
///
/// The math mirrors the `streaming_budget` harness in
/// `world::manager` exactly — sort ascending, then index with
/// `round((len - 1) * p)` — so the two evidence surfaces cannot disagree
/// about what "p95" means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameBudgetStats {
    pub samples: usize,
    pub p50: Duration,
    pub p95: Duration,
    pub max: Duration,
}

/// Pure percentile helper: `None` for an empty sample so callers never print
/// a digest full of zeros (and never divide by zero).
pub fn frame_budget_stats(samples: &[Duration]) -> Option<FrameBudgetStats> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort();
    let pct = |p: f64| -> Duration {
        let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
        sorted[idx]
    };
    Some(FrameBudgetStats {
        samples: sorted.len(),
        p50: pct(0.50),
        p95: pct(0.95),
        max: *sorted.last().expect("non-empty sample"),
    })
}

/// Pure formatter for the one digest line printed on demo-seam shutdown
/// (D086). `None` for an empty sample: a demo that never saw a frame has no
/// budget to report and must not print a misleading all-zero line.
pub fn format_frame_budget_digest(samples: &[Duration]) -> Option<String> {
    let stats = frame_budget_stats(samples)?;
    Some(format!(
        "frame_budget: samples={} p50={:.3}ms p95={:.3}ms max={:.3}ms target: p95<={:.1}ms",
        stats.samples,
        stats.p50.as_secs_f64() * 1000.0,
        stats.p95.as_secs_f64() * 1000.0,
        stats.max.as_secs_f64() * 1000.0,
        FRAME_BUDGET_TARGET_MS,
    ))
}

/// Bounded per-frame wall-time sampler for the demo-shutdown digest.
///
/// HONESTY NOTE (S08-T01): the samples are real wall dt from the frame loop.
/// Default settings run `vsync = false`, which requests the uncapped
/// `Immediate` present mode (`render::context::preferred_present_mode`), so a
/// frame's dt measures the work of that frame rather than a display refresh
/// wait. The p95 <= 16.6 ms claim is therefore only meaningful at default
/// settings; a vsync-on run is paced by the compositor and would report the
/// refresh interval, not the render cost.
#[derive(Debug, Default)]
pub struct FrameBudgetSampler {
    pub samples: Vec<Duration>,
}

impl FrameBudgetSampler {
    pub fn new() -> Self {
        Self {
            samples: Vec::with_capacity(FRAME_BUDGET_SAMPLE_CAP),
        }
    }

    /// Record one frame's wall dt. Silently drops samples past the cap: the
    /// digest is a percentile summary, so a longer run only loses tail
    /// resolution, never accuracy for the already-recorded window.
    pub fn push(&mut self, dt: Duration) {
        if self.samples.len() < FRAME_BUDGET_SAMPLE_CAP {
            self.samples.push(dt);
        }
    }

    /// Drop the sample window — called at session transitions so one digest
    /// covers exactly one run/world.
    pub fn reset(&mut self) {
        self.samples.clear();
    }

    /// Accessor kept `pub` for the dual-stage digest tests; production code
    /// reads the sample count through the digest itself.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn digest(&self) -> Option<String> {
        format_frame_budget_digest(&self.samples)
    }
}

/// Sample one frame: only Playing frames are recorded (menu/settings/pause
/// frames are not gameplay work and must not pad the milestone claim).
pub fn sample_frame_budget(sampler: &mut FrameBudgetSampler, playing: bool, dt: Duration) {
    if playing {
        sampler.push(dt);
    }
}

/// The shutdown digest is gated on a demo seam having run (D086) and on a
/// non-empty sample window — the crate stays log-free (D006/MEM009) outside
/// that one bounded diagnostic seam.
pub fn frame_budget_digest_for_shutdown(
    demo_seam_ran: bool,
    sampler: &FrameBudgetSampler,
) -> Option<String> {
    if !demo_seam_ran {
        return None;
    }
    sampler.digest()
}

/// Extra wall-clock allowance on top of one accelerated cycle before the
/// bounded demo shuts down even if no redraws are being delivered.
pub const DEMO_SHUTDOWN_GRACE_SECS: f32 = 5.0;

/// Wall-clock safety net for a bounded accelerated run: `demo_days` full
/// cycles at the given scale plus a small grace for startup. The frame-driven
/// [`DemoCycleWatch`] is the primary completion signal; this deadline keeps
/// the run bounded even if the compositor stops delivering redraws.
/// `None` at the default scale (a normal run is unbounded).
pub fn demo_shutdown_deadline(time_scale: f32, demo_days: u32) -> Option<Instant> {
    if time_scale <= DEFAULT_TIME_SCALE {
        return None;
    }
    let cycle_secs = daycycle::DAY_SECONDS / time_scale;
    let demo_secs = cycle_secs * demo_days.max(1) as f32;
    Some(Instant::now() + Duration::from_secs_f32(demo_secs + DEMO_SHUTDOWN_GRACE_SECS))
}
