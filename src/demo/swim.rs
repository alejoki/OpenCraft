//! Scripted ocean-crossing demo seam (`--demo-swim`).
//!
//! A pure move out of the bin crate (S09): the route resolver, the bounded
//! stage watcher and the per-frame driver. `scripts/swim_smoke.sh` runs this
//! seam through the real binary and pins its digest line.

use std::time::Instant;

use glam::Vec3;

use crate::player::physics::{InputState, PLAYER_HALF_WIDTH};
use crate::world::block::BlockId;
use crate::world::terrain::{biome_at_seeded, terrain_height_at_seeded, Biome, SEA_LEVEL};
use crate::WorldSession;

// ---------------------------------------------------------------------------
// Scripted ocean-crossing demo (`--demo-swim`, S08-T02)
// ---------------------------------------------------------------------------

/// Forward axis of the scripted swim. S03 proved the crossing on +Z from the
/// origin for seed 12345; the seam deliberately pins the same axis so the
/// headless proof and the real-binary seam describe one route.
pub const SWIM_AXIS: &str = "+Z";

/// Yaw whose flat forward vector is exactly +Z (`(cos yaw, 0, sin yaw)`).
pub const SWIM_YAW: f32 = std::f32::consts::FRAC_PI_2;

/// Spawn height above the sea level plane (S03 drops the body from here into
/// the ocean column; 3 blocks clears the water surface at `SEA_LEVEL + 1`).
pub const SWIM_START_ABOVE_SEA: f32 = 3.0;

/// Bounded scan windows for the route: an ocean column within 512 blocks of
/// the origin along +Z, and the first land column within 4096 blocks after it.
/// Identical to the S03 acceptance proof's scan, so the route the demo swims
/// is the route the headless test bounds.
pub const SWIM_OCEAN_SCAN_BLOCKS: i32 = 512;
pub const SWIM_SHORE_SCAN_BLOCKS: i32 = 4096;

/// Top face of the water column (`SEA_LEVEL` is the last water block).
pub const SWIM_WATER_TOP: f32 = SEA_LEVEL as f32 + 1.0;

/// How far above the spawn column's seabed a sinking or swimming body must
/// stay. Mirrors the S03 `SEABED_MARGIN`, so "the body sank" means the same
/// thing in both surfaces.
pub const SWIM_SEABED_MARGIN: f32 = 2.0;

/// Stage budgets mirrored from the S03 harness's frame caps at its fixed
/// 1/60 s step (600 entry / 60 sink / 120 rise / 9000 crossing frames).
///
/// They are expressed in seconds of simulated time rather than frames because
/// the production frame loop is uncapped: a frame budget that is safe at 60 Hz
/// would cut a 300 fps run off after a fraction of the same simulated motion.
/// The watcher accumulates the same `dt` the player update consumes (clamped
/// exactly like `Player::update` clamps it), so the bound is frame-rate
/// independent and still frames the S03 phase boundaries.
pub const SWIM_ENTRY_SECS: f32 = 10.0;
/// D087 sink proof window: released idle just inside the water, the body must
/// descend away from the surface rather than being held there.
pub const SWIM_SINK_SECS: f32 = 1.0;
/// Minimum idle descent (blocks) that counts as a real sink inside the window.
pub const SWIM_SINK_MIN_DROP: f32 = 1.5;
pub const SWIM_RISE_SECS: f32 = 2.0;
/// The S03 crossing cap (9000 frames at 1/60 s) plus a production margin: the
/// real binary's frame rate is unbounded, so the same simulated crossing is
/// fewer wall seconds and a single long hitch must not eat the budget. The
/// 360 s wall-clock deadline, not this cap, remains the hard bound on a stalled
/// run, and gated (non-resident) frames never charge this budget.
pub const SWIM_CROSSING_SECS: f32 = 180.0;

/// Mirrors `Player::update`'s per-step clamp, so the watcher's simulated-time
/// budget advances exactly as far as the physics did.
pub const SWIM_STEP_CAP: f32 = 0.1;

/// Wall-clock safety net for the scripted swim, independent of `time_scale`:
/// the accelerated-demo deadline only arms above 1x, and the swim deliberately
/// runs at the default scale (no seasonal terrain rollover mid-crossing).
pub const SWIM_DEADLINE_SECS: u64 = 360;

/// The scripted crossing's pinned geometry, resolved once at startup from the
/// same pure terrain functions the chunk generator uses.
///
/// For seed 12345 this is the S03 route: an Ocean column at the world origin
/// (`h = 8` after the ocean-depth bonus) and the first land column at or
/// above `SEA_LEVEL` along +Z.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SwimRoute {
    pub seed: u32,
    /// Feet position the fresh-world launch teleports to (column centre, above
    /// the water surface, facing +Z).
    pub start: Vec3,
    /// Column index along +Z where the ocean starts (the route's near end).
    pub ocean_start_z: i32,
    /// Column index along +Z of the first land at or above `SEA_LEVEL` (the
    /// far shore the demo must reach).
    pub land_z: i32,
    /// Terrain height of the spawn column, below `SEA_LEVEL`; the sink guard's
    /// reference.
    pub seabed_height: i32,
}

impl SwimRoute {
    /// Crossing length in blocks: ocean start column to far-shore column.
    pub fn distance(&self) -> i32 {
        self.land_z - self.ocean_start_z
    }
}

/// Resolve the scripted crossing for `seed` with no chunks allocated: scan +Z
/// from the origin for an Ocean column below `SEA_LEVEL`, then for the first
/// column at or above `SEA_LEVEL` (the far shore). `None` when either bound is
/// missing, so a seed without a usable crossing produces an honest abort
/// digest instead of swimming at nothing.
pub fn swim_route(seed: u32) -> Option<SwimRoute> {
    let ocean_start_z = (0..SWIM_OCEAN_SCAN_BLOCKS).find(|&d| {
        biome_at_seeded(0, d, seed) == Biome::Ocean
            && terrain_height_at_seeded(0, d, seed) < SEA_LEVEL
    })?;
    let land_z = (ocean_start_z + 1..SWIM_SHORE_SCAN_BLOCKS)
        .find(|&d| terrain_height_at_seeded(0, d, seed) >= SEA_LEVEL)?;
    let seabed_height = terrain_height_at_seeded(0, ocean_start_z, seed);
    Some(SwimRoute {
        seed,
        start: Vec3::new(
            0.5,
            SEA_LEVEL as f32 + SWIM_START_ABOVE_SEA,
            ocean_start_z as f32 + 0.5,
        ),
        ocean_start_z,
        land_z,
        seabed_height,
    })
}

/// Teleport a freshly constructed session onto the scripted route: column
/// centre, above the water surface, facing +Z, walk mode on (a flying body
/// would never enter the water). Loaded saves never call this — the fresh-world
/// startup branch is the only caller.
pub fn arm_swim_route(session: &mut WorldSession, route: SwimRoute) {
    session.player.position = route.start;
    session.player.velocity = Vec3::ZERO;
    session.player.yaw = SWIM_YAW;
    session.player.pitch = 0.0;
    session.player.on_ground = false;
    session.player.fly_mode = false;
    session.sync_camera();
}

/// The S03 phases reworked for D087: entry, idle sink, Jump rise, crossing,
/// and the terminal landed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwimStage {
    Entry,
    Sink,
    Rise,
    Crossing,
    Landed,
}

impl SwimStage {
    /// The stage name used in abort diagnostics and by the headless stage
    /// tests, so a failure names the same phase the headless proof names.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Entry => "entry",
            Self::Sink => "sink",
            Self::Rise => "rise",
            Self::Crossing => "crossing",
            Self::Landed => "landed",
        }
    }
}

/// One frame's verdict from the stage watcher.
#[derive(Debug, PartialEq, Eq)]
pub enum SwimProgress {
    Continue,
    Landed,
    Aborted {
        stage: &'static str,
        observed: String,
    },
}

/// The scripted input for a stage, as a constructed copy.
///
/// The seam never mutates the application's input field: the real input path stays
/// byte-identical, and this copy is what a bounded phase supplies instead.
/// Entry and sink are idle (D087: water must sink the body unaided), rise
/// holds `jump_or_up`, and crossing holds `forward` plus a bounded jump
/// cadence: Jump engages while the body's feet are at or below the top face of
/// the water column and releases as soon as it clears it. The stage itself is
/// the bounded cadence: each phase has a budget, so no key can be held past its
/// window.
pub fn swim_scripted_input(stage: SwimStage, feet_y: f32) -> InputState {
    match stage {
        SwimStage::Rise => InputState {
            jump_or_up: true,
            ..InputState::default()
        },
        SwimStage::Crossing => InputState {
            forward: true,
            jump_or_up: feet_y <= SWIM_WATER_TOP,
            ..InputState::default()
        },
        SwimStage::Entry | SwimStage::Sink | SwimStage::Landed => InputState::default(),
    }
}

/// Bounded, stage-naming watcher for the scripted crossing — the swim twin of
/// [`crate::demo::watch::DemoCycleWatch`].
///
/// It observes the production player after every scripted `Player::update`
/// call and advances through the D087 phases: entry (the feet block becomes
/// water), sink (idle descent away from the surface, never `on_ground`), rise
/// (`jump_or_up` clears the water surface), crossing (reach the land target
/// standing on land above `SEA_LEVEL` with a bounded jump cadence). Every abort
/// names the stage and the observed values, so a stalled run is diagnosable
/// from its one digest line.
#[derive(Debug)]
pub struct SwimWatch {
    pub route: SwimRoute,
    pub stage: SwimStage,
    /// Scripted frames stepped (the digest's `frames` value). Warm-up frames
    /// spent waiting for the spawn column to stream in are deliberately not
    /// counted: they are not frames of the crossing.
    pub frames: usize,
    /// Simulated seconds spent in the current stage.
    pub stage_elapsed: f32,
    pub sink_start_y: f32,
    pub sink_min_y: f32,
    pub rise_max_y: f32,
    pub rise_submerged_frames: usize,
    pub crossing_water_frames: usize,
}

impl SwimWatch {
    pub fn new(route: SwimRoute) -> Self {
        Self {
            route,
            stage: SwimStage::Entry,
            frames: 0,
            stage_elapsed: 0.0,
            sink_start_y: f32::MAX,
            sink_min_y: f32::MAX,
            rise_max_y: f32::MIN,
            rise_submerged_frames: 0,
            crossing_water_frames: 0,
        }
    }

    pub fn input(&self, position: Vec3) -> InputState {
        swim_scripted_input(self.stage, position.y)
    }

    pub fn advance(&mut self, stage: SwimStage) {
        self.stage = stage;
        self.stage_elapsed = 0.0;
    }

    /// `sink` cell: `start->lowest` once the sink phase began, `none` before.
    pub fn sink_band(&self) -> String {
        if self.sink_start_y >= self.sink_min_y && self.sink_start_y < f32::MAX {
            format!("{:.3}->{:.3}", self.sink_start_y, self.sink_min_y)
        } else {
            "none".to_string()
        }
    }

    /// `rise` cell: the highest feet position reached while rising, if any.
    pub fn rise_peak(&self) -> String {
        if self.rise_max_y > f32::MIN {
            format!("{:.3}", self.rise_max_y)
        } else {
            "none".to_string()
        }
    }

    pub fn head(&self) -> String {
        format!(
            "swim_demo: seed={} axis={} distance={} frames={} sink={} rise={}",
            self.route.seed,
            SWIM_AXIS,
            self.route.distance(),
            self.frames,
            self.sink_band(),
            self.rise_peak(),
        )
    }

    /// The success digest, printed once on the demo-shutdown path.
    pub fn digest(&self) -> String {
        format!("{} landed=true", self.head())
    }

    /// The self-describing abort digest: same fields plus the stage and the
    /// values observed at the failure.
    pub fn abort_digest(&self, stage: &'static str, observed: &str) -> String {
        format!(
            "{} landed=false stage={} observed={}",
            self.head(),
            stage,
            observed
        )
    }

    /// A compact "what the player was doing" cell for abort messages.
    pub fn observed(position: Vec3, on_ground: bool, feet_liquid: bool) -> String {
        format!(
            "y={:.3} z={:.3} on_ground={} feet_liquid={}",
            position.y, position.z, on_ground, feet_liquid
        )
    }

    /// Feed one scripted frame's post-physics state. `step` is the simulated
    /// seconds the player update consumed (already clamped like the physics).
    pub fn observe(
        &mut self,
        step: f32,
        position: Vec3,
        on_ground: bool,
        feet_liquid: bool,
    ) -> SwimProgress {
        self.frames += 1;
        self.stage_elapsed += step;
        let seabed_floor = self.route.seabed_height as f32 + SWIM_SEABED_MARGIN;
        let observed = Self::observed(position, on_ground, feet_liquid);
        match self.stage {
            SwimStage::Entry => {
                if feet_liquid {
                    self.sink_start_y = position.y;
                    self.sink_min_y = position.y;
                    self.advance(SwimStage::Sink);
                    return SwimProgress::Continue;
                }
                if position.y <= seabed_floor {
                    return SwimProgress::Aborted {
                        stage: "entry",
                        observed: format!(
                            "sank below the seabed margin before the feet block became water; \
                             {observed} seabed={} floor={seabed_floor:.3}",
                            self.route.seabed_height
                        ),
                    };
                }
                if self.stage_elapsed >= SWIM_ENTRY_SECS {
                    return SwimProgress::Aborted {
                        stage: "entry",
                        observed: format!(
                            "feet block never became water within {SWIM_ENTRY_SECS:.1}s; {observed}"
                        ),
                    };
                }
                SwimProgress::Continue
            }
            SwimStage::Sink => {
                self.sink_min_y = self.sink_min_y.min(position.y);
                if on_ground {
                    return SwimProgress::Aborted {
                        stage: "sink",
                        observed: format!(
                            "an idle sinking body touched ground (it walked the seabed); {observed}"
                        ),
                    };
                }
                if position.y <= seabed_floor {
                    return SwimProgress::Aborted {
                        stage: "sink",
                        observed: format!(
                            "sank below the seabed margin while proving the idle sink; {observed} \
                             seabed={} floor={seabed_floor:.3}",
                            self.route.seabed_height
                        ),
                    };
                }
                if self.stage_elapsed >= SWIM_SINK_SECS {
                    let dropped = self.sink_start_y - self.sink_min_y;
                    if dropped < SWIM_SINK_MIN_DROP {
                        return SwimProgress::Aborted {
                            stage: "sink",
                            observed: format!(
                                "idle body only dropped {dropped:.3} blocks in \
                                 {SWIM_SINK_SECS:.1}s (expected at least \
                                 {SWIM_SINK_MIN_DROP:.1}); D087 requires water to sink it away from \
                                 the surface; {observed}"
                            ),
                        };
                    }
                    self.advance(SwimStage::Rise);
                }
                SwimProgress::Continue
            }
            SwimStage::Rise => {
                self.rise_max_y = self.rise_max_y.max(position.y);
                if feet_liquid {
                    self.rise_submerged_frames += 1;
                }
                if on_ground {
                    return SwimProgress::Aborted {
                        stage: "rise",
                        observed: format!("a rising body touched ground; {observed}"),
                    };
                }
                if position.y > SWIM_WATER_TOP {
                    self.advance(SwimStage::Crossing);
                    return SwimProgress::Continue;
                }
                if self.stage_elapsed >= SWIM_RISE_SECS {
                    return SwimProgress::Aborted {
                        stage: "rise",
                        observed: format!(
                            "jump_or_up never cleared the water surface y>{SWIM_WATER_TOP:.3} \
                             within {SWIM_RISE_SECS:.1}s; {observed} peak={:.3} submerged_frames={}",
                            self.rise_max_y, self.rise_submerged_frames
                        ),
                    };
                }
                SwimProgress::Continue
            }
            SwimStage::Crossing => {
                if feet_liquid {
                    self.crossing_water_frames += 1;
                }
                if position.y <= seabed_floor {
                    return SwimProgress::Aborted {
                        stage: "crossing",
                        observed: format!(
                            "sank below the seabed margin while crossing; {observed} \
                             seabed={} floor={seabed_floor:.3}",
                            self.route.seabed_height
                        ),
                    };
                }
                if on_ground
                    && position.y <= SEA_LEVEL as f32
                    && position.z + PLAYER_HALF_WIDTH < self.route.land_z as f32
                {
                    return SwimProgress::Aborted {
                        stage: "crossing",
                        observed: format!(
                            "touched ground inside the water column while its body box was still \
                             short of the far shore (walked the seabed); {observed} land_z={}",
                            self.route.land_z
                        ),
                    };
                }
                if position.z >= self.route.land_z as f32
                    && on_ground
                    && position.y > SEA_LEVEL as f32
                {
                    self.advance(SwimStage::Landed);
                    return SwimProgress::Landed;
                }
                if self.stage_elapsed >= SWIM_CROSSING_SECS {
                    return SwimProgress::Aborted {
                        stage: "crossing",
                        observed: format!(
                            "never stood on the far shore z>={} above SEA_LEVEL {SEA_LEVEL} \
                             within {SWIM_CROSSING_SECS:.1}s; {observed} water_frames={}",
                            self.route.land_z, self.crossing_water_frames
                        ),
                    };
                }
                SwimProgress::Continue
            }
            SwimStage::Landed => SwimProgress::Landed,
        }
    }
}

/// The `--demo-swim` seam's whole state: the resolved route, the armed watcher,
/// the one latched digest, and the wall-clock deadline.
#[derive(Debug)]
pub struct SwimDemo {
    pub seed: u32,
    pub route: Option<SwimRoute>,
    pub watch: Option<SwimWatch>,
    /// The digest latched when the run ends (landing or abort). `about_to_wait`
    /// prints it together with the frame-budget line, so the crate stays
    /// log-free between demo seams (D006/MEM009, D086).
    pub digest: Option<String>,
    /// Whether the latched outcome was an abort. `main` turns this into a
    /// non-zero process status, so a bounded smoke can tell "ran and failed"
    /// from "was killed by the harness timeout".
    pub failed: bool,
    /// Wall-clock safety net, armed only for a direct-to-gameplay launch.
    pub deadline: Option<Instant>,
}

impl SwimDemo {
    pub fn new(seed: u32, route: Option<SwimRoute>, deadline: Option<Instant>) -> Self {
        Self {
            seed,
            route,
            watch: None,
            digest: None,
            failed: false,
            deadline,
        }
    }

    pub fn finished(&self) -> bool {
        self.digest.is_some()
    }

    pub fn finish_landed(&mut self) {
        if self.finished() {
            return;
        }
        if let Some(watch) = &self.watch {
            self.digest = Some(watch.digest());
        }
    }

    /// Latch a self-describing abort. Idempotent: the first outcome wins, so a
    /// deadline that fires on the same frame as a stage abort cannot overwrite
    /// the more specific message.
    pub fn abort(&mut self, stage: &'static str, observed: &str) {
        if self.finished() {
            return;
        }
        self.digest = Some(match &self.watch {
            Some(watch) => watch.abort_digest(stage, observed),
            None => format!(
                "swim_demo: seed={} axis={} distance=0 frames=0 sink=none rise=none \
                 landed=false stage={} observed={}",
                self.seed, SWIM_AXIS, stage, observed
            ),
        });
        self.failed = true;
    }
}

/// Drive one Playing frame of the scripted swim through the production player
/// update seam. Returns true when this frame ended the run (landed or
/// aborted), so the caller latches the bounded-demo shutdown.
///
/// The first call waits (bounded by the swim deadline) for the spawn column's
/// water block to stream in before arming the watcher. This is the production
/// analogue of S03's warm-up gate: `get_block` answers Air for a non-resident
/// chunk, and falling through that Air would look like a body that sank
/// through the ocean. No scripted input is fed while waiting.
pub fn swim_frame(demo: &mut SwimDemo, session: &mut WorldSession, dt: f32) -> bool {
    if demo.finished() {
        return true;
    }
    let Some(route) = demo.route else {
        demo.abort("route", "no_ocean_crossing_for_this_seed");
        return true;
    };
    if demo.watch.is_none() {
        let water = session.world.get_block(0, SEA_LEVEL, route.ocean_start_z);
        if water != BlockId::Water {
            return false;
        }
        demo.watch = Some(SwimWatch::new(route));
    }
    let watch = demo
        .watch
        .as_mut()
        .expect("watch is armed immediately above");
    // Streaming gate, mirroring the S03 harness's per-step residency gate: a
    // non-resident chunk reads as Air, which silently removes the water branch
    // (drag, sink, and rise) and drops the scripted body out of the crossing.
    // the body to stream in before feeding any scripted input. The wall-clock
    // deadline bounds the wait, and a gated frame is not a crossing frame: no
    // physics runs and no simulated time is charged to the stage budget.
    let col_x = session.player.position.x.floor() as i32;
    let col_z = session.player.position.z.floor() as i32;
    if terrain_height_at_seeded(col_x, col_z, route.seed) < SEA_LEVEL
        && session.world.get_block(col_x, SEA_LEVEL, col_z) != BlockId::Water
    {
        return false;
    }
    let input = watch.input(session.player.position);
    session
        .player
        .update(dt, &input, |x, y, z| session.world.get_block(x, y, z));
    let feet_liquid = session
        .world
        .get_block(
            session.player.position.x.floor() as i32,
            (session.player.position.y + 1e-4).floor() as i32,
            session.player.position.z.floor() as i32,
        )
        .is_liquid();
    let progress = watch.observe(
        dt.clamp(0.0, SWIM_STEP_CAP),
        session.player.position,
        session.player.on_ground,
        feet_liquid,
    );
    match progress {
        SwimProgress::Continue => false,
        SwimProgress::Landed => {
            demo.finish_landed();
            true
        }
        SwimProgress::Aborted { stage, observed } => {
            demo.abort(stage, &observed);
            true
        }
    }
}
