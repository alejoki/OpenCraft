use glam::IVec3;

use crate::world::block::BlockId;

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum MiningTick {
    Idle,
    Mining { progress: f32 },
    Broke { pos: IVec3, block: BlockId },
}

#[derive(Copy, Clone, Debug)]
pub struct MiningState {
    target: Option<IVec3>,
    block: Option<BlockId>,
    elapsed: f32,
    duration: f32,
}

impl MiningState {
    pub const fn new() -> Self {
        Self {
            target: None,
            block: None,
            elapsed: 0.0,
            duration: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.target = None;
        self.block = None;
        self.elapsed = 0.0;
        self.duration = 0.0;
    }

    pub fn progress(&self) -> Option<f32> {
        match (self.target, self.block) {
            (Some(_), Some(_)) if self.duration > 0.0 => {
                Some((self.elapsed / self.duration).clamp(0.0, 1.0))
            }
            _ => None,
        }
    }

    pub fn tick(&mut self, dt: f32, candidate: Option<(IVec3, BlockId)>) -> MiningTick {
        let dt = if !dt.is_finite() || dt <= 0.0 {
            0.0
        } else {
            dt.min(0.1)
        };

        let Some((pos, block)) = candidate else {
            self.reset();
            return MiningTick::Idle;
        };
        // Unbreakable blocks (Air, Bedrock) behave exactly like no candidate.
        let Some(duration) = block.break_duration_secs() else {
            self.reset();
            return MiningTick::Idle;
        };

        if self.target != Some(pos) || self.block != Some(block) {
            self.target = Some(pos);
            self.block = Some(block);
            self.elapsed = 0.0;
            self.duration = duration;
        }

        self.elapsed += dt;
        if self.elapsed >= self.duration {
            let broke = MiningTick::Broke { pos, block };
            self.reset();
            return broke;
        }

        MiningTick::Mining {
            progress: self.progress().unwrap_or(0.0),
        }
    }
}

impl Default for MiningState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POS: IVec3 = IVec3::new(0, 40, 0);

    fn tick_stone(mining: &mut MiningState, ticks: u32) -> MiningTick {
        let mut last = MiningTick::Idle;
        for _ in 0..ticks {
            last = mining.tick(0.1, Some((POS, BlockId::Stone)));
        }
        last
    }

    #[test]
    fn no_candidate_is_idle() {
        let mut mining = MiningState::new();
        assert_eq!(mining.tick(0.1, None), MiningTick::Idle);
        assert!(mining.progress().is_none());
    }

    #[test]
    fn unbreakable_candidate_is_idle_and_never_accumulates() {
        let mut mining = MiningState::new();
        for _ in 0..100 {
            assert_eq!(
                mining.tick(0.1, Some((POS, BlockId::Bedrock))),
                MiningTick::Idle
            );
            assert!(mining.progress().is_none());
        }
        assert_eq!(
            mining.tick(0.1, Some((POS, BlockId::Air))),
            MiningTick::Idle
        );
    }

    #[test]
    fn stone_breaks_exactly_once_after_sustained_hold() {
        let mut mining = MiningState::new();
        match tick_stone(&mut mining, 6) {
            MiningTick::Mining { progress } => {
                assert!((progress - 0.5).abs() < 1e-3, "progress was {progress}");
            }
            other => panic!("expected Mining, got {other:?}"),
        }
        // f32 accumulation can land a hair below 1.2s, so allow an extra
        // frame of hold; the important contract is exactly one Broke.
        let mut broke = false;
        for _ in 0..10 {
            match mining.tick(0.1, Some((POS, BlockId::Stone))) {
                MiningTick::Broke { pos, block } => {
                    assert_eq!((pos, block), (POS, BlockId::Stone));
                    assert!(!broke, "Broke must be emitted exactly once");
                    broke = true;
                    break;
                }
                MiningTick::Mining { .. } => assert!(!broke, "must not mine after a break"),
                MiningTick::Idle => panic!("a held, breakable block must not go Idle"),
            }
        }
        assert!(broke, "stone must break after a sustained hold");
        // Releasing (or losing the target) after the break is Idle with no progress.
        assert_eq!(mining.tick(0.1, None), MiningTick::Idle);
        assert!(mining.progress().is_none());
    }

    #[test]
    fn huge_dt_is_clamped_to_a_tenth_of_a_second() {
        let mut mining = MiningState::new();
        match mining.tick(5.0, Some((POS, BlockId::Stone))) {
            MiningTick::Mining { progress } => {
                assert!(progress <= 0.084, "clamped progress was {progress}");
            }
            other => panic!("expected Mining, got {other:?}"),
        }
    }

    #[test]
    fn changing_position_restarts_progress() {
        let mut mining = MiningState::new();
        mining.tick(0.6, Some((POS, BlockId::Stone)));
        let moved = IVec3::new(1, 40, 0);
        match mining.tick(0.1, Some((moved, BlockId::Stone))) {
            MiningTick::Mining { progress } => {
                assert!(progress <= 0.1 / 1.2 + 1e-4, "progress was {progress}");
            }
            other => panic!("expected Mining, got {other:?}"),
        }
    }

    #[test]
    fn changing_block_with_equal_duration_restarts_progress() {
        let mut mining = MiningState::new();
        let mut before = 0.0;
        for _ in 0..3 {
            before = match mining.tick(0.1, Some((POS, BlockId::Dirt))) {
                MiningTick::Mining { progress } => progress,
                other => panic!("expected Mining, got {other:?}"),
            };
        }
        // Dirt and Sand both take 0.45s: only the block identity can reset progress.
        match mining.tick(0.1, Some((POS, BlockId::Sand))) {
            MiningTick::Mining { progress } => {
                assert!(progress <= 0.1 / 0.45 + 1e-4, "progress was {progress}");
                assert!(progress < before, "progress must restart, was {progress}");
            }
            other => panic!("expected Mining, got {other:?}"),
        }
    }

    #[test]
    fn nan_and_negative_dt_add_nothing() {
        let mut mining = MiningState::new();
        match mining.tick(f32::NAN, Some((POS, BlockId::Stone))) {
            MiningTick::Mining { progress } => assert_eq!(progress, 0.0),
            other => panic!("expected Mining, got {other:?}"),
        }
        match mining.tick(-1.0, Some((POS, BlockId::Stone))) {
            MiningTick::Mining { progress } => assert_eq!(progress, 0.0),
            other => panic!("expected Mining, got {other:?}"),
        }
        match mining.tick(f32::INFINITY, Some((POS, BlockId::Stone))) {
            MiningTick::Mining { progress } => {
                assert!(progress <= 0.084, "clamped progress was {progress}");
            }
            other => panic!("expected Mining, got {other:?}"),
        }
    }

    #[test]
    fn release_after_partial_progress_resets() {
        let mut mining = MiningState::new();
        mining.tick(0.5, Some((POS, BlockId::Stone)));
        assert!(mining.progress().is_some());
        assert_eq!(mining.tick(0.1, None), MiningTick::Idle);
        assert!(mining.progress().is_none());
    }
}
