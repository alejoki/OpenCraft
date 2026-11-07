//! Pure, GPU-free wheat crop growth.
//!
//! The visible stage is already persisted as an ordinary v6 chunk edit: the
//! wheat stage block ids 17..=20 live in the same `ChunkEditMap` as every
//! other player edit, so save/reload and chunk unload/regeneration carry the
//! crop with no new save surface. Growth *progress* is deliberately ephemeral:
//! [`CropRuntime`] only tracks how long each resident crop has been growing,
//! rebuilds its candidates from chunk edits at load, and restarts every one at
//! zero. Growth progress stays out of the save and a crop cannot bank
//! progress while its chunk is unloaded.
//!
//! The runtime takes the world through two closures (`resident` and
//! `block_at`) instead of importing a session or renderer type, so stage math,
//! residency gating, support checks, and deterministic ordering are all
//! testable without a window or GPU.

use glam::IVec3;
use rustc_hash::FxHashMap;

use crate::world::block::BlockId;
use crate::world::chunk::{local_coords_from_index, CHUNK_SIZE};
use crate::world::persistence::ChunkEditMap;

/// Game seconds one wheat stage takes at the default time scale.
///
/// Four stages therefore take 240 game seconds: four real minutes at 1x, or
/// one real second per stage under `--time-scale 60`.
pub const WHEAT_STAGE_GAME_SECS: f32 = 60.0;

/// Creator-tunable crop growth cadence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FarmingRules {
    /// Game seconds between wheat growth stages.
    pub wheat_stage_game_secs: f32,
}

impl Default for FarmingRules {
    fn default() -> Self {
        Self {
            wheat_stage_game_secs: WHEAT_STAGE_GAME_SECS,
        }
    }
}

/// One crop that crossed at least one stage during a [`CropRuntime::tick`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CropAdvance {
    /// World position of the crop block.
    pub pos: IVec3,
    /// The live block seen when this tick started.
    pub from: BlockId,
    /// The stage reached after consuming this tick's growth.
    pub to: BlockId,
}

/// Ephemeral growth progress for every immature wheat crop.
///
/// The map is keyed by world position and bounded by player-planted crops; it
/// is rebuilt from chunk edits at load rather than scanned per frame.
#[derive(Debug, Default)]
pub struct CropRuntime {
    growth: FxHashMap<IVec3, f32>,
    rules: FarmingRules,
}

impl CropRuntime {
    /// Rebuilds candidates from persisted chunk edits.
    ///
    /// Only immature wheat (stages 0-2) becomes a candidate: mature stage 3
    /// needs no growth, and every non-wheat edit is ignored. Each candidate
    /// starts at progress `0.0` because progress is not persisted.
    pub fn from_edits(edits: &ChunkEditMap) -> Self {
        let mut runtime = Self::default();
        for (chunk, blocks) in edits {
            for (&index, &block) in blocks {
                if !block.is_wheat_stage() || block == BlockId::WheatStage3 {
                    continue;
                }
                let (x, y, z) = local_coords_from_index(index);
                let chunk_size = CHUNK_SIZE as i32;
                let pos = IVec3::new(
                    chunk.x * chunk_size + x as i32,
                    chunk.y * chunk_size + y as i32,
                    chunk.z * chunk_size + z as i32,
                );
                runtime.growth.insert(pos, 0.0);
            }
        }
        runtime
    }

    /// Adds (or restarts) a candidate at progress `0.0`.
    pub fn insert(&mut self, pos: IVec3) {
        self.growth.insert(pos, 0.0);
    }

    /// Forgets a candidate, e.g. after its block was broken or replaced.
    pub fn remove(&mut self, pos: IVec3) {
        self.growth.remove(&pos);
    }

    /// The crop growth tuning this runtime simulates with.
    pub fn rules(&self) -> FarmingRules {
        self.rules
    }

    /// Installs live crop growth tuning without touching banked progress.
    ///
    /// A non-finite or non-positive stage duration is ignored so a malformed
    /// reload can neither stall growth nor make every crop mature at once;
    /// the previous duration stays in force.
    pub fn set_rules(&mut self, rules: FarmingRules) {
        if !rules.wheat_stage_game_secs.is_finite() || rules.wheat_stage_game_secs <= 0.0 {
            return;
        }
        self.rules = rules;
    }

    /// Number of tracked candidates.
    pub fn len(&self) -> usize {
        self.growth.len()
    }

    /// Whether no candidates are tracked.
    pub fn is_empty(&self) -> bool {
        self.growth.is_empty()
    }

    /// Current unspent growth progress for `pos`, if tracked.
    pub fn progress(&self, pos: IVec3) -> Option<f32> {
        self.growth.get(&pos).copied()
    }

    /// Advances every resident, supported, immature crop by `game_dt`.
    ///
    /// Semantics, in order:
    ///
    /// - Non-finite or non-positive `game_dt` is a no-op.
    /// - Candidates are visited in sorted `(x, y, z)` order so equal inputs
    ///   produce an identical advance list.
    /// - A crop in a non-resident chunk is retained and skipped; unloading
    ///   pauses growth rather than forgetting the planting.
    /// - A crop whose live block is no longer wheat, or is already mature
    ///   stage 3, is removed.
    /// - A crop in a resident chunk whose resident support block is not
    ///   [`BlockId::Farmland`] is removed. A non-resident support chunk is not
    ///   read and not treated as missing support.
    /// - While `paused` (Winter), a valid crop keeps its progress unchanged.
    /// - Otherwise progress grows by `game_dt`, one configured
    ///   [`FarmingRules::wheat_stage_game_secs`] (default
    ///   [`WHEAT_STAGE_GAME_SECS`]) is consumed per stage, and crops that
    ///   crossed at least one stage produce one [`CropAdvance`] with their
    ///   final stage. Reaching stage 3 removes the candidate; otherwise the
    ///   leftover progress is retained.
    pub fn tick<F, G>(
        &mut self,
        game_dt: f32,
        paused: bool,
        resident: F,
        block_at: G,
    ) -> Vec<CropAdvance>
    where
        F: Fn(IVec3) -> bool,
        G: Fn(IVec3) -> BlockId,
    {
        if !game_dt.is_finite() || game_dt <= 0.0 {
            return Vec::new();
        }

        let mut positions: Vec<IVec3> = self.growth.keys().copied().collect();
        positions.sort_unstable_by_key(|pos| (pos.x, pos.y, pos.z));

        let mut advances = Vec::new();
        let mut stale = Vec::new();
        for pos in positions {
            if !resident(pos) {
                continue;
            }

            let live = block_at(pos);
            if !live.is_wheat_stage() || live == BlockId::WheatStage3 {
                stale.push(pos);
                continue;
            }

            // Only probe the support block when its chunk is loaded; a chunk
            // boundary must never delete a crop because the ground is out of
            // view.
            let below = pos - IVec3::Y;
            if resident(below) && block_at(below) != BlockId::Farmland {
                stale.push(pos);
                continue;
            }

            if paused {
                continue;
            }

            // Copy the live stage duration before taking the mutable progress
            // borrow: `set_rules` may have changed it since the last tick.
            let stage_secs = self.rules.wheat_stage_game_secs;
            let Some(progress) = self.growth.get_mut(&pos) else {
                continue;
            };
            *progress += game_dt;

            // Integer stage division keeps a huge finite dt from looping once
            // per elapsed stage; `next_wheat_stage` still owns stage order.
            let stage_steps = (*progress / stage_secs).floor() as u32;
            let mut to = live;
            let mut advanced_stages = 0u32;
            while advanced_stages < stage_steps {
                let Some(next) = to.next_wheat_stage() else {
                    break;
                };
                to = next;
                advanced_stages += 1;
            }
            if advanced_stages > 0 {
                *progress -= advanced_stages as f32 * stage_secs;
                advances.push(CropAdvance {
                    pos,
                    from: live,
                    to,
                });
            }
            if to == BlockId::WheatStage3 {
                stale.push(pos);
            }
        }

        for pos in stale {
            self.growth.remove(&pos);
        }
        advances
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::world::chunk::{linear_index, ChunkPos};

    #[test]
    fn farming_rules_default_matches_constants() {
        assert_eq!(
            FarmingRules::default().wheat_stage_game_secs,
            WHEAT_STAGE_GAME_SECS
        );
    }

    fn add_edit(
        edits: &mut ChunkEditMap,
        chunk: ChunkPos,
        (x, y, z): (usize, usize, usize),
        block: BlockId,
    ) {
        edits
            .entry(chunk)
            .or_default()
            .insert(linear_index(x, y, z) as u16, block);
    }

    fn runtime_at(positions: &[IVec3]) -> CropRuntime {
        let mut runtime = CropRuntime::default();
        for &pos in positions {
            runtime.insert(pos);
        }
        runtime
    }

    /// World lookup for one crop sitting on Farmland: `live` at the crop and
    /// Farmland directly below it.
    fn one_crop_on_farmland(crop: IVec3, live: BlockId) -> impl Fn(IVec3) -> BlockId {
        move |pos| {
            if pos == crop - IVec3::Y {
                BlockId::Farmland
            } else {
                live
            }
        }
    }

    #[test]
    fn from_edits_keeps_only_immature_wheat_and_converts_world_positions() {
        let mut edits = ChunkEditMap::default();
        add_edit(
            &mut edits,
            ChunkPos::new(0, 0, 0),
            (2, 3, 4),
            BlockId::WheatStage0,
        );
        add_edit(
            &mut edits,
            ChunkPos::new(0, 0, 0),
            (5, 6, 7),
            BlockId::WheatStage2,
        );
        add_edit(
            &mut edits,
            ChunkPos::new(0, 0, 0),
            (1, 1, 1),
            BlockId::WheatStage3,
        );
        add_edit(
            &mut edits,
            ChunkPos::new(0, 0, 0),
            (8, 8, 8),
            BlockId::Farmland,
        );
        add_edit(
            &mut edits,
            ChunkPos::new(-1, 2, -3),
            (31, 0, 1),
            BlockId::WheatStage1,
        );

        let runtime = CropRuntime::from_edits(&edits);
        assert_eq!(runtime.len(), 3);
        assert_eq!(runtime.progress(IVec3::new(2, 3, 4)), Some(0.0));
        assert_eq!(runtime.progress(IVec3::new(5, 6, 7)), Some(0.0));
        assert_eq!(runtime.progress(IVec3::new(-1, 64, -95)), Some(0.0));
        assert_eq!(runtime.progress(IVec3::new(1, 1, 1)), None);
        assert_eq!(runtime.progress(IVec3::new(8, 8, 8)), None);
    }

    #[test]
    fn from_edits_on_an_empty_map_is_empty() {
        let runtime = CropRuntime::from_edits(&ChunkEditMap::default());
        assert!(runtime.is_empty());
    }

    #[test]
    fn insert_remove_and_progress_are_position_keyed() {
        let pos = IVec3::new(-4, 20, 7);
        let other = IVec3::new(0, 0, 0);
        let mut runtime = CropRuntime::default();
        assert!(runtime.is_empty());
        runtime.insert(pos);
        runtime.insert(other);
        assert_eq!(runtime.len(), 2);
        assert_eq!(runtime.progress(pos), Some(0.0));
        runtime.remove(pos);
        assert_eq!(runtime.len(), 1);
        assert_eq!(runtime.progress(pos), None);
        runtime.remove(pos);
        assert_eq!(runtime.len(), 1, "removing an absent crop is a no-op");
    }

    #[test]
    fn tick_advances_exactly_one_stage_at_stage_seconds() {
        let pos = IVec3::new(3, 4, 5);
        let mut runtime = runtime_at(&[pos]);
        let advances = runtime.tick(
            60.0,
            false,
            |_| true,
            one_crop_on_farmland(pos, BlockId::WheatStage0),
        );
        assert_eq!(
            advances,
            vec![CropAdvance {
                pos,
                from: BlockId::WheatStage0,
                to: BlockId::WheatStage1,
            }]
        );
        assert_eq!(runtime.progress(pos), Some(0.0));
        assert_eq!(runtime.len(), 1);
    }

    #[test]
    fn tick_uses_the_configured_stage_seconds() {
        let pos = IVec3::new(3, 4, 5);

        // The default 60 s stage means 10 game seconds are not enough.
        let mut default_runtime = runtime_at(&[pos]);
        assert_eq!(
            default_runtime.rules(),
            FarmingRules::default(),
            "a new runtime starts on the pinned default"
        );
        assert!(default_runtime
            .tick(
                10.0,
                false,
                |_| true,
                one_crop_on_farmland(pos, BlockId::WheatStage0)
            )
            .is_empty());
        assert_eq!(default_runtime.progress(pos), Some(10.0));

        // A fresh runtime with a tuned 10 s stage advances in the same time.
        let mut tuned = runtime_at(&[pos]);
        tuned.set_rules(FarmingRules {
            wheat_stage_game_secs: 10.0,
        });
        assert_eq!(tuned.rules().wheat_stage_game_secs, 10.0);
        let advances = tuned.tick(
            10.0,
            false,
            |_| true,
            one_crop_on_farmland(pos, BlockId::WheatStage0),
        );
        assert_eq!(
            advances,
            vec![CropAdvance {
                pos,
                from: BlockId::WheatStage0,
                to: BlockId::WheatStage1,
            }]
        );
        assert_eq!(tuned.progress(pos), Some(0.0));
    }

    #[test]
    fn set_rules_ignores_non_finite_and_non_positive_stage_seconds() {
        let pos = IVec3::new(0, 0, 0);
        let mut runtime = runtime_at(&[pos]);
        runtime.set_rules(FarmingRules {
            wheat_stage_game_secs: 10.0,
        });
        for bad in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            runtime.set_rules(FarmingRules {
                wheat_stage_game_secs: bad,
            });
            assert_eq!(
                runtime.rules().wheat_stage_game_secs,
                10.0,
                "stage seconds {bad} must be ignored"
            );
        }
        // The retained 10 s duration still advances; a zero duration would
        // otherwise mature the crop on any positive tick.
        let advances = runtime.tick(
            10.0,
            false,
            |_| true,
            one_crop_on_farmland(pos, BlockId::WheatStage0),
        );
        assert_eq!(
            advances,
            vec![CropAdvance {
                pos,
                from: BlockId::WheatStage0,
                to: BlockId::WheatStage1,
            }]
        );
    }

    #[test]
    fn tick_carries_the_unspent_remainder() {
        let pos = IVec3::new(0, 0, 0);
        let mut runtime = runtime_at(&[pos]);
        let advances = runtime.tick(
            90.0,
            false,
            |_| true,
            one_crop_on_farmland(pos, BlockId::WheatStage0),
        );
        assert_eq!(
            advances,
            vec![CropAdvance {
                pos,
                from: BlockId::WheatStage0,
                to: BlockId::WheatStage1,
            }]
        );
        assert_eq!(runtime.progress(pos), Some(30.0));

        let advances = runtime.tick(
            30.0,
            false,
            |_| true,
            one_crop_on_farmland(pos, BlockId::WheatStage1),
        );
        assert_eq!(
            advances,
            vec![CropAdvance {
                pos,
                from: BlockId::WheatStage1,
                to: BlockId::WheatStage2,
            }]
        );
        assert_eq!(runtime.progress(pos), Some(0.0));
    }

    #[test]
    fn a_large_tick_jumps_from_stage_zero_to_maturity_and_removes_the_candidate() {
        let pos = IVec3::new(1, 2, 3);
        let mut runtime = runtime_at(&[pos]);
        let advances = runtime.tick(
            200.0,
            false,
            |_| true,
            one_crop_on_farmland(pos, BlockId::WheatStage0),
        );
        assert_eq!(
            advances,
            vec![CropAdvance {
                pos,
                from: BlockId::WheatStage0,
                to: BlockId::WheatStage3,
            }]
        );
        assert!(runtime.is_empty());
        assert_eq!(runtime.progress(pos), None);
    }

    #[test]
    fn mature_and_replaced_crops_never_advance() {
        let mature = IVec3::new(0, 0, 0);
        let replaced = IVec3::new(4, 0, 0);
        let mut runtime = runtime_at(&[mature, replaced]);
        let advances = runtime.tick(
            600.0,
            false,
            |_| true,
            |pos| {
                if pos == mature {
                    BlockId::WheatStage3
                } else {
                    BlockId::Farmland
                }
            },
        );
        assert!(advances.is_empty());
        assert!(runtime.is_empty());
    }

    #[test]
    fn winter_pause_retains_progress_and_candidate_until_spring() {
        let pos = IVec3::new(8, 9, 10);
        let mut runtime = runtime_at(&[pos]);
        assert!(runtime
            .tick(
                30.0,
                false,
                |_| true,
                one_crop_on_farmland(pos, BlockId::WheatStage0),
            )
            .is_empty());
        assert_eq!(runtime.progress(pos), Some(30.0));

        assert!(runtime
            .tick(
                500.0,
                true,
                |_| true,
                one_crop_on_farmland(pos, BlockId::WheatStage0),
            )
            .is_empty());
        assert_eq!(runtime.progress(pos), Some(30.0));
        assert_eq!(runtime.len(), 1);

        let advances = runtime.tick(
            30.0,
            false,
            |_| true,
            one_crop_on_farmland(pos, BlockId::WheatStage0),
        );
        assert_eq!(
            advances,
            vec![CropAdvance {
                pos,
                from: BlockId::WheatStage0,
                to: BlockId::WheatStage1,
            }]
        );
        assert_eq!(runtime.progress(pos), Some(0.0));
    }

    #[test]
    fn non_resident_candidates_are_retained_and_resume_when_loaded() {
        let pos = IVec3::new(1, 2, 3);
        let mut runtime = runtime_at(&[pos]);
        assert!(runtime
            .tick(120.0, false, |_| false, |_| BlockId::WheatStage0)
            .is_empty());
        assert_eq!(runtime.len(), 1);
        assert_eq!(runtime.progress(pos), Some(0.0));

        let advances = runtime.tick(
            60.0,
            false,
            |_| true,
            one_crop_on_farmland(pos, BlockId::WheatStage0),
        );
        assert_eq!(
            advances,
            vec![CropAdvance {
                pos,
                from: BlockId::WheatStage0,
                to: BlockId::WheatStage1,
            }]
        );
    }

    #[test]
    fn broken_replaced_and_unsupported_crops_are_removed() {
        let broken = IVec3::new(1, 10, 1);
        let replaced = IVec3::new(5, 10, 5);
        let unsupported = IVec3::new(9, 10, 9);
        let mut runtime = runtime_at(&[broken, replaced, unsupported]);

        let advances = runtime.tick(
            60.0,
            false,
            |_| true,
            |pos| {
                if pos == broken {
                    BlockId::Air
                } else if pos == replaced {
                    BlockId::Grass
                } else if pos == unsupported - IVec3::Y {
                    BlockId::Dirt
                } else {
                    BlockId::WheatStage0
                }
            },
        );
        assert!(advances.is_empty());
        assert!(runtime.is_empty());
    }

    #[test]
    fn a_non_resident_support_chunk_does_not_remove_the_crop() {
        let pos = IVec3::new(0, 0, 0);
        let below = pos - IVec3::Y;
        let mut runtime = runtime_at(&[pos]);
        let advances = runtime.tick(
            60.0,
            false,
            move |candidate| candidate == pos,
            move |candidate| {
                assert_ne!(
                    candidate, below,
                    "support block must not be read from a non-resident chunk"
                );
                BlockId::WheatStage0
            },
        );
        assert_eq!(
            advances,
            vec![CropAdvance {
                pos,
                from: BlockId::WheatStage0,
                to: BlockId::WheatStage1,
            }]
        );
        assert_eq!(runtime.len(), 1);
    }

    #[test]
    fn invalid_dt_is_a_no_op() {
        let pos = IVec3::new(2, 2, 2);
        for bad in [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            0.0,
            -1.0,
            -f32::MIN_POSITIVE,
        ] {
            let mut runtime = runtime_at(&[pos]);
            let advances = runtime.tick(bad, false, |_| true, |_| BlockId::WheatStage0);
            assert!(advances.is_empty(), "dt {bad} must not advance");
            assert_eq!(runtime.len(), 1, "dt {bad} must not remove candidates");
            assert_eq!(runtime.progress(pos), Some(0.0), "dt {bad} must not move");
        }
    }

    #[test]
    fn tick_order_is_deterministic_and_sorted() {
        let positions = [
            IVec3::new(5, 0, 0),
            IVec3::new(1, 0, 0),
            IVec3::new(1, 0, -2),
            IVec3::new(1, -1, 3),
        ];
        let run = || {
            let mut runtime = runtime_at(&positions);
            runtime.tick(
                60.0,
                false,
                |_| true,
                |pos| {
                    if positions.iter().any(|crop| pos == *crop - IVec3::Y) {
                        BlockId::Farmland
                    } else {
                        BlockId::WheatStage0
                    }
                },
            )
        };

        let first = run();
        let second = run();
        assert_eq!(first, second, "equal inputs must produce equal advances");
        let order: Vec<IVec3> = first.iter().map(|advance| advance.pos).collect();
        assert_eq!(
            order,
            vec![
                IVec3::new(1, -1, 3),
                IVec3::new(1, 0, -2),
                IVec3::new(1, 0, 0),
                IVec3::new(5, 0, 0),
            ]
        );
    }
}
