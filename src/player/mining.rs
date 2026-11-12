use glam::IVec3;

use crate::content::ContentRegistry;
use crate::player::inventory::Item;
use crate::player::tools::ToolRules;
use crate::world::block::BlockId;

/// A validated block-breaking target.
///
/// The effective duration is carried *with* the target instead of being
/// re-derived from the block, so tool rules live in [`crate::player::tools`]
/// and the accumulator only integrates what it was handed. Construction is
/// fallible on purpose: unbreakable blocks and non-finite/non-positive
/// durations can never become a target, so no caller can mine Air or Bedrock
/// by inventing a duration.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MiningTarget {
    pos: IVec3,
    block: BlockId,
    duration: f32,
}

impl MiningTarget {
    /// Validates an explicit duration for `block` at `pos`.
    ///
    /// Returns `None` when the block is structurally unbreakable or the
    /// duration is not a finite positive number.
    pub fn new(pos: IVec3, block: BlockId, duration: f32) -> Option<Self> {
        Self::new_with(ContentRegistry::builtin(), pos, block, duration)
    }

    /// Registry-aware [`MiningTarget::new`].
    ///
    /// Breakability is the registry definition's authored hardness, so a pack
    /// block with authored hardness is valid while a placeholder with
    /// `hardness: None` stays unbreakable.
    pub fn new_with(
        registry: &ContentRegistry,
        pos: IVec3,
        block: BlockId,
        duration: f32,
    ) -> Option<Self> {
        if !duration.is_finite() || duration <= 0.0 {
            return None;
        }
        // Internal catalog hardness guard: the block's authorized hardness is
        // the authority on breakability, never the caller-supplied duration.
        registry.block(block)?.hardness?;
        Some(Self {
            pos,
            block,
            duration,
        })
    }

    /// Derives a target from the block's base hardness and the selected item.
    ///
    /// This is the production entry point: the item's tool class and tier
    /// decide the effective duration via [`tools::break_duration`].
    pub fn from_block(pos: IVec3, block: BlockId, selected_item: Option<Item>) -> Option<Self> {
        Self::from_block_with(ContentRegistry::builtin(), pos, block, selected_item)
    }

    /// Registry-aware [`MiningTarget::from_block`] for pack-authored blocks.
    pub fn from_block_with(
        registry: &ContentRegistry,
        pos: IVec3,
        block: BlockId,
        selected_item: Option<Item>,
    ) -> Option<Self> {
        Self::from_block_with_rules(registry, pos, block, selected_item, &ToolRules::default())
    }

    /// [`MiningTarget::from_block_with`] with live built-in tier tuning.
    ///
    /// The duration still comes from the registry, so a pack block keeps its
    /// authored hardness and a pack tool keeps its authored tier speed. Only
    /// the built-in wood/stone/iron multipliers follow `rules`.
    pub fn from_block_with_rules(
        registry: &ContentRegistry,
        pos: IVec3,
        block: BlockId,
        selected_item: Option<Item>,
        rules: &ToolRules,
    ) -> Option<Self> {
        Self::new_with(
            registry,
            pos,
            block,
            registry.break_duration_with_rules(block, selected_item, rules)?,
        )
    }

    pub const fn pos(self) -> IVec3 {
        self.pos
    }

    pub const fn block(self) -> BlockId {
        self.block
    }

    pub const fn duration(self) -> f32 {
        self.duration
    }
}

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

    pub fn tick(&mut self, dt: f32, candidate: Option<MiningTarget>) -> MiningTick {
        self.tick_with(ContentRegistry::builtin(), dt, candidate)
    }

    /// Registry-aware [`MiningState::tick`].
    ///
    /// Breakability is re-read from the registry definition's authored
    /// hardness, so a pack block with authored hardness keeps accumulating
    /// instead of being reset by the built-in-only accessor.
    pub fn tick_with(
        &mut self,
        registry: &ContentRegistry,
        dt: f32,
        candidate: Option<MiningTarget>,
    ) -> MiningTick {
        let dt = if !dt.is_finite() || dt <= 0.0 {
            0.0
        } else {
            dt.min(0.1)
        };

        let Some(target) = candidate else {
            self.reset();
            return MiningTick::Idle;
        };
        // Fail closed even though `MiningTarget::new` already validated: the
        // catalog hardness guard stays the authority on breakability, and a
        // degenerate duration must never accumulate progress.
        if registry
            .block(target.block)
            .and_then(|def| def.hardness)
            .is_none()
            || !target.duration.is_finite()
            || target.duration <= 0.0
        {
            self.reset();
            return MiningTick::Idle;
        }

        // Position, block, and effective duration are all part of the reset
        // key: switching tools mid-mine must not inherit progress.
        if self.target != Some(target.pos)
            || self.block != Some(target.block)
            || self.duration != target.duration
        {
            self.target = Some(target.pos);
            self.block = Some(target.block);
            self.elapsed = 0.0;
            self.duration = target.duration;
        }

        self.elapsed += dt;
        if self.elapsed >= self.duration {
            let broke = MiningTick::Broke {
                pos: target.pos,
                block: target.block,
            };
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
    use crate::player::inventory::{ItemId, ToolKind, ToolTier};

    const POS: IVec3 = IVec3::new(0, 40, 0);

    fn bare(block: BlockId) -> MiningTarget {
        MiningTarget::from_block(POS, block, None).expect("breakable block is a valid target")
    }

    fn at(pos: IVec3, block: BlockId) -> MiningTarget {
        MiningTarget::from_block(pos, block, None).expect("breakable block is a valid target")
    }

    fn tooled(pos: IVec3, block: BlockId, kind: ToolKind, tier: ToolTier) -> MiningTarget {
        let item = Item::new(ItemId::Tool { kind, tier }, 1).expect("valid tool");
        MiningTarget::from_block(pos, block, Some(item)).expect("breakable block is a valid target")
    }

    fn tick_stone(mining: &mut MiningState, ticks: u32) -> MiningTick {
        let mut last = MiningTick::Idle;
        for _ in 0..ticks {
            last = mining.tick(0.1, Some(bare(BlockId::Stone)));
        }
        last
    }

    fn hold_until_broke(mining: &mut MiningState, target: MiningTarget) -> u32 {
        for tick in 1..=200 {
            match mining.tick(0.1, Some(target)) {
                MiningTick::Broke { pos, block } => {
                    assert_eq!(pos, target.pos());
                    assert_eq!(block, target.block());
                    return tick;
                }
                MiningTick::Mining { .. } => {}
                MiningTick::Idle => panic!("a held, breakable target must not go Idle"),
            }
        }
        panic!("target never broke");
    }

    fn hold_until_broke_with(
        mining: &mut MiningState,
        registry: &ContentRegistry,
        target: MiningTarget,
    ) -> u32 {
        for tick in 1..=200 {
            match mining.tick_with(registry, 0.1, Some(target)) {
                MiningTick::Broke { pos, block } => {
                    assert_eq!(pos, target.pos());
                    assert_eq!(block, target.block());
                    return tick;
                }
                MiningTick::Mining { .. } => {}
                MiningTick::Idle => panic!("a held, breakable target must not go Idle"),
            }
        }
        panic!("target never broke");
    }

    /// A one-block pack registry with authored hardness and a ruby item drop.
    fn ruby_registry() -> ContentRegistry {
        use crate::content::spec::{BlockEntry, BlockTextures, ItemEntry, PackAsset};
        use crate::content::{build_registry, PackContentSpec, PackIdentity};

        let mut spec = PackContentSpec::default();
        spec.blocks.push(BlockEntry {
            id: "mymod:ruby_ore".to_string(),
            display_name: None,
            textures: BlockTextures {
                all: Some("ruby_ore.png".to_string()),
                ..BlockTextures::default()
            },
            hardness: 0.3,
            drop: Some("mymod:ruby".to_string()),
            tool: None,
            stack_limit: 64,
        });
        spec.items.push(ItemEntry {
            id: "mymod:ruby".to_string(),
            display_name: None,
            icon: "ruby.png".to_string(),
            stack_limit: 32,
            food: None,
        });
        for path in ["ruby_ore.png", "ruby.png"] {
            spec.assets.insert(
                path.to_string(),
                PackAsset {
                    rgba: vec![0x22; 1024],
                    source_side: 16,
                },
            );
        }

        let packs = vec![(
            PackIdentity {
                folder: "mymod-ruby".to_string(),
                version: None,
            },
            spec,
        )];
        let (registry, report) = build_registry(ContentRegistry::builtin(), &packs);
        assert_eq!(report.errors(), 0, "fixture registry is clean: {report:?}");
        registry
    }

    #[test]
    fn from_block_with_rules_uses_tuned_durations_and_default_wrapper_pins_old_ones() {
        let registry = ContentRegistry::builtin();
        let pickaxe = Item::new(
            ItemId::Tool {
                kind: ToolKind::Pickaxe,
                tier: ToolTier::Stone,
            },
            1,
        )
        .expect("valid pickaxe");
        let rules = ToolRules {
            hand_speed_multiplier: 1.0,
            wood_speed_multiplier: 8.0,
            stone_speed_multiplier: 12.0,
            iron_speed_multiplier: 16.0,
        };

        let tuned = MiningTarget::from_block_with_rules(
            registry,
            POS,
            BlockId::Stone,
            Some(pickaxe),
            &rules,
        )
        .expect("stone is breakable");
        assert_eq!(tuned.duration(), 1.2 / 12.0);

        let default = MiningTarget::from_block_with(registry, POS, BlockId::Stone, Some(pickaxe))
            .expect("stone is breakable");
        assert_eq!(default.duration(), 1.2 / 4.0);

        // A zero multiplier clamps to bare hand instead of creating a
        // zero/infinity duration that `new_with` would reject or mis-time.
        let zero = ToolRules {
            hand_speed_multiplier: 0.0,
            wood_speed_multiplier: 0.0,
            stone_speed_multiplier: 0.0,
            iron_speed_multiplier: 0.0,
        };
        let clamped = MiningTarget::from_block_with_rules(
            registry,
            POS,
            BlockId::Stone,
            Some(pickaxe),
            &zero,
        )
        .expect("a clamped multiplier still yields a valid target");
        assert_eq!(clamped.duration(), 1.2);

        assert!(MiningTarget::from_block_with_rules(
            registry,
            POS,
            BlockId::Bedrock,
            Some(pickaxe),
            &rules
        )
        .is_none());
    }

    #[test]
    fn registry_modded_block_is_mineable_through_tick_with() {
        let registry = ruby_registry();
        let def = registry
            .blocks()
            .iter()
            .find(|def| def.name == "mymod:ruby_ore")
            .expect("fixture block is registered");
        let block = def.id;

        let target = MiningTarget::from_block_with(&registry, POS, block, None)
            .expect("authored hardness makes the pack block a target");
        assert_eq!(target.duration(), 0.3);
        assert!(
            MiningTarget::from_block(POS, block, None).is_none(),
            "the built-in registry cannot see pack ids"
        );

        // The built-in accumulator has no definition for the id and resets,
        // which is exactly the pre-registry bug this task fixes.
        let mut mining = MiningState::new();
        assert_eq!(mining.tick(0.1, Some(target)), MiningTick::Idle);
        assert!(mining.progress().is_none());

        let ticks = hold_until_broke_with(&mut mining, &registry, target);
        assert!(
            ticks.abs_diff(3) <= 1,
            "0.3s at 0.1s per tick broke after {ticks} ticks"
        );
    }

    #[test]
    fn placeholder_block_stays_unbreakable_through_registry_guards() {
        let mut registry = ruby_registry();
        let placeholder = registry.append_placeholder_block("mymod:missing");

        assert!(registry.block(placeholder).is_some());
        assert!(MiningTarget::new_with(&registry, POS, placeholder, 0.5).is_none());
        assert!(MiningTarget::from_block_with(&registry, POS, placeholder, None).is_none());

        // A forged/unresolved id with no definition is equally unbreakable.
        let unknown = BlockId::Modded(u16::MAX);
        assert!(MiningTarget::new_with(&registry, POS, unknown, 0.5).is_none());
    }

    #[test]
    fn no_candidate_is_idle() {
        let mut mining = MiningState::new();
        assert_eq!(mining.tick(0.1, None), MiningTick::Idle);
        assert!(mining.progress().is_none());
    }

    #[test]
    fn unbreakable_blocks_never_become_targets() {
        for block in [BlockId::Air, BlockId::Bedrock] {
            assert!(
                MiningTarget::new(POS, block, 0.1).is_none(),
                "{block:?} must reject even an explicit duration"
            );
            assert!(MiningTarget::from_block(POS, block, None).is_none());
            let pickaxe = Item::new(
                ItemId::Tool {
                    kind: ToolKind::Pickaxe,
                    tier: ToolTier::Iron,
                },
                1,
            )
            .expect("valid tool");
            assert!(
                MiningTarget::from_block(POS, block, Some(pickaxe)).is_none(),
                "{block:?} must stay unbreakable with an iron pickaxe"
            );
        }
    }

    #[test]
    fn invalid_durations_are_rejected() {
        for duration in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(
                MiningTarget::new(POS, BlockId::Stone, duration).is_none(),
                "duration {duration} must be rejected"
            );
        }
        assert!(MiningTarget::new(POS, BlockId::Stone, 0.001).is_some());
    }

    #[test]
    fn unbreakable_or_missing_candidate_resets_without_progress() {
        let mut mining = MiningState::new();
        // No candidate at all: exactly the Idle/reset contract.
        for _ in 0..100 {
            assert_eq!(mining.tick(0.1, None), MiningTick::Idle);
            assert!(mining.progress().is_none());
        }
        // A candidate that a caller could not validate (Air/Bedrock) has no
        // constructor path, so the observable equivalent is `None`.
        assert!(MiningTarget::new(POS, BlockId::Bedrock, 0.1).is_none());
        assert!(MiningTarget::new(POS, BlockId::Air, 0.1).is_none());
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
            match mining.tick(0.1, Some(bare(BlockId::Stone))) {
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
    fn repeated_ticks_after_a_break_restart_instead_of_rebreaking() {
        let mut mining = MiningState::new();
        let target = bare(BlockId::Stone);
        hold_until_broke(&mut mining, target);
        // The same target held again starts a fresh cycle: the first tick is
        // Mining (tiny progress), never a second immediate Broke.
        match mining.tick(0.1, Some(target)) {
            MiningTick::Mining { progress } => {
                assert!(progress <= 0.1 / 1.2 + 1e-4, "progress was {progress}");
            }
            other => panic!("expected a fresh Mining cycle, got {other:?}"),
        }
    }

    #[test]
    fn huge_dt_is_clamped_to_a_tenth_of_a_second() {
        let mut mining = MiningState::new();
        match mining.tick(5.0, Some(bare(BlockId::Stone))) {
            MiningTick::Mining { progress } => {
                assert!(progress <= 0.084, "clamped progress was {progress}");
            }
            other => panic!("expected Mining, got {other:?}"),
        }
    }

    #[test]
    fn changing_position_restarts_progress() {
        let mut mining = MiningState::new();
        mining.tick(0.6, Some(bare(BlockId::Stone)));
        let moved = IVec3::new(1, 40, 0);
        match mining.tick(0.1, Some(at(moved, BlockId::Stone))) {
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
            before = match mining.tick(0.1, Some(bare(BlockId::Dirt))) {
                MiningTick::Mining { progress } => progress,
                other => panic!("expected Mining, got {other:?}"),
            };
        }
        // Dirt and Sand both take 0.45s: only the block identity can reset progress.
        match mining.tick(0.1, Some(bare(BlockId::Sand))) {
            MiningTick::Mining { progress } => {
                assert!(progress <= 0.1 / 0.45 + 1e-4, "progress was {progress}");
                assert!(progress < before, "progress must restart, was {progress}");
            }
            other => panic!("expected Mining, got {other:?}"),
        }
    }

    #[test]
    fn changing_effective_duration_restarts_progress() {
        let mut mining = MiningState::new();
        for _ in 0..5 {
            mining.tick(0.1, Some(bare(BlockId::Stone)));
        }
        let before = mining.progress().expect("partial bare-hand progress");
        assert!((before - 0.5 / 1.2).abs() < 1e-3, "progress was {before}");

        // Same position and block, but a wood pickaxe halves the duration.
        // The reset key includes duration, so progress cannot be inherited.
        let wood = tooled(POS, BlockId::Stone, ToolKind::Pickaxe, ToolTier::Wood);
        assert_eq!(wood.duration(), 0.6);
        match mining.tick(0.1, Some(wood)) {
            MiningTick::Mining { progress } => {
                assert!(progress <= 0.1 / 0.6 + 1e-4, "progress was {progress}");
                assert!(progress < before, "progress must restart, was {progress}");
            }
            other => panic!("expected Mining, got {other:?}"),
        }
    }

    #[test]
    fn matching_tools_break_at_their_exact_adjusted_time() {
        // Stone base 1.2s: wood 0.6s = 6 ticks, stone 0.3s = 3 ticks,
        // iron 0.2s = 2 ticks at 0.1s per tick.
        for (tier, expected_ticks) in [
            (ToolTier::Wood, 6),
            (ToolTier::Stone, 3),
            (ToolTier::Iron, 2),
        ] {
            let mut mining = MiningState::new();
            let target = tooled(POS, BlockId::Stone, ToolKind::Pickaxe, tier);
            let ticks = hold_until_broke(&mut mining, target);
            assert!(
                ticks.abs_diff(expected_ticks) <= 1,
                "{tier:?} broke after {ticks} ticks, expected {expected_ticks}"
            );
        }
    }

    #[test]
    fn mismatched_tool_keeps_base_timing_and_does_not_reset_progress() {
        let mut mining = MiningState::new();
        for _ in 0..5 {
            mining.tick(0.1, Some(bare(BlockId::Stone)));
        }
        let before = mining.progress().expect("partial progress");
        // An axe on stone has the same effective duration as a bare hand, so
        // the accumulator keeps integrating rather than restarting.
        let axe = tooled(POS, BlockId::Stone, ToolKind::Axe, ToolTier::Iron);
        assert_eq!(axe.duration(), 1.2);
        match mining.tick(0.1, Some(axe)) {
            MiningTick::Mining { progress } => {
                assert!(progress > before, "progress must continue, was {progress}");
            }
            other => panic!("expected Mining, got {other:?}"),
        }
    }

    #[test]
    fn nan_and_negative_dt_add_nothing() {
        let mut mining = MiningState::new();
        match mining.tick(f32::NAN, Some(bare(BlockId::Stone))) {
            MiningTick::Mining { progress } => assert_eq!(progress, 0.0),
            other => panic!("expected Mining, got {other:?}"),
        }
        match mining.tick(-1.0, Some(bare(BlockId::Stone))) {
            MiningTick::Mining { progress } => assert_eq!(progress, 0.0),
            other => panic!("expected Mining, got {other:?}"),
        }
        match mining.tick(f32::INFINITY, Some(bare(BlockId::Stone))) {
            MiningTick::Mining { progress } => {
                assert!(progress <= 0.084, "clamped progress was {progress}");
            }
            other => panic!("expected Mining, got {other:?}"),
        }
    }

    #[test]
    fn release_after_partial_progress_resets() {
        let mut mining = MiningState::new();
        mining.tick(0.5, Some(bare(BlockId::Stone)));
        assert!(mining.progress().is_some());
        assert_eq!(mining.tick(0.1, None), MiningTick::Idle);
        assert!(mining.progress().is_none());
    }
}
