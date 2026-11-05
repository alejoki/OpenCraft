//! Transient selected-item name lifetime.
//!
//! Pure timer/state: no GPU, window, or clock. `main.rs` feeds the selected
//! hotbar slot and its stack once per Playing frame, ticks it with the frame
//! delta, and draws the label only while [`SelectionLabel::is_visible`] holds.
//!
//! The label restarts whenever the selection changes — a different slot or a
//! different stack (identity, count, or durability) — and fades smoothly to
//! zero over [`SELECTED_NAME_SECS`].

use crate::content::ContentRegistry;
use crate::player::inventory::Item;
use crate::ui::hud::selected_item_label_with;

/// Seconds the selected-item label stays on screen after a change.
pub const SELECTED_NAME_SECS: f32 = 2.0;

/// Creator-tunable selected-item label lifetime.
///
/// Kept in the UI module so the render.toml `ui.selected_name_secs` row does
/// not move UI timing into `world::daycycle`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectionRules {
    /// Seconds the selected-item name stays visible after a change.
    pub selected_name_secs: f32,
}

impl Default for SelectionRules {
    fn default() -> Self {
        Self {
            selected_name_secs: SELECTED_NAME_SECS,
        }
    }
}

/// Selected hotbar slot plus the fade timer for its transient name.
///
/// `new` starts empty and hidden: observing `(0, None)` is not a change, so a
/// freshly launched game does not flash an "Empty" label for doing nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectionLabel {
    slot: usize,
    item: Option<Item>,
    remaining: f32,
}

impl SelectionLabel {
    /// Hidden label on slot 0 with no item.
    pub const fn new() -> Self {
        Self {
            slot: 0,
            item: None,
            remaining: 0.0,
        }
    }

    /// Reports whether `(slot, item)` differs from the observed selection.
    ///
    /// When it differs the state is adopted and the timer restarts at
    /// [`SELECTED_NAME_SECS`]. Item equality covers identity, count, and
    /// durability, so picking up or consuming part of a stack restarts the
    /// label too.
    pub fn observe(&mut self, slot: usize, item: Option<Item>) -> bool {
        let changed = self.slot != slot || self.item != item;
        if changed {
            self.slot = slot;
            self.item = item;
            self.remaining = SELECTED_NAME_SECS;
        }
        changed
    }

    /// Advances the fade timer by one frame delta.
    ///
    /// Negative and non-finite deltas are ignored (a paused/corrupted clock
    /// must not resurrect or permanently hide the label), and the remainder is
    /// clamped to `0.0..=SELECTED_NAME_SECS`.
    pub fn tick(&mut self, dt: f32) {
        if !dt.is_finite() || dt <= 0.0 {
            return;
        }
        self.remaining = (self.remaining - dt).clamp(0.0, SELECTED_NAME_SECS);
    }

    /// Smooth fade factor in `0.0..=1.0`, zero once the timer expires.
    ///
    /// Smoothstep keeps the appearance and disappearance soft at both ends
    /// without any extra state.
    pub fn alpha(&self) -> f32 {
        let t = (self.remaining / SELECTED_NAME_SECS).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    /// Whether the label should be drawn this frame.
    ///
    /// An empty selected slot is never visible, even if the timer was
    /// restarted by a slot change onto nothing.
    pub fn is_visible(&self) -> bool {
        self.item.is_some() && self.remaining > 0.0
    }

    /// Label text, or `None` while hidden.
    pub fn label(&self) -> Option<String> {
        self.label_with(None)
    }

    /// Registry-aware [`Self::label`]: pack items and tools show their authored
    /// display name instead of the static `Modded Item` fallback.
    pub fn label_with(&self, registry: Option<&ContentRegistry>) -> Option<String> {
        if !self.is_visible() {
            return None;
        }
        self.item
            .map(|item| selected_item_label_with(Some(item), registry))
    }

    /// Selected slot last observed (useful for diagnostics/tests).
    pub const fn slot(&self) -> usize {
        self.slot
    }

    /// Remaining fade time in seconds (useful for diagnostics/tests).
    pub const fn remaining(&self) -> f32 {
        self.remaining
    }
}

impl Default for SelectionLabel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::inventory::{ItemId, Material, ToolKind, ToolTier};
    use crate::world::block::BlockId;

    #[test]
    fn selection_rules_default_matches_constants() {
        assert_eq!(
            SelectionRules::default().selected_name_secs,
            SELECTED_NAME_SECS
        );
    }

    fn stone(count: u16) -> Item {
        Item::new(ItemId::Block(BlockId::Stone), count).expect("stone")
    }

    fn pickaxe(durability: u16) -> Item {
        Item::tool_with_durability(ToolKind::Pickaxe, ToolTier::Iron, durability).expect("pickaxe")
    }

    #[test]
    fn new_label_is_hidden_and_observing_the_initial_selection_is_not_a_change() {
        let mut label = SelectionLabel::new();
        assert!(!label.is_visible());
        assert_eq!(label.label(), None);
        assert_eq!(label.alpha(), 0.0);
        assert!(!label.observe(0, None), "initial state is already observed");
        assert!(!label.is_visible());
    }

    #[test]
    fn registry_aware_label_names_a_missing_pack_item_by_its_saved_name() {
        let mut registry = ContentRegistry::builtin().clone();
        let index = registry.append_placeholder_item("mymod:ruby");
        let item = Item::new_with(&registry, ItemId::Item(index), 1).expect("placeholder item");
        let mut label = SelectionLabel::new();
        assert!(label.observe(0, Some(item)));
        assert_eq!(
            label.label_with(Some(&registry)).as_deref(),
            Some("mymod:ruby x1")
        );
    }

    #[test]
    fn slot_and_stack_changes_restart_the_timer() {
        let mut label = SelectionLabel::new();
        assert!(label.observe(3, Some(stone(5))), "first selection changes");
        assert!(label.is_visible());
        assert_eq!(label.remaining(), SELECTED_NAME_SECS);
        assert_eq!(label.alpha(), 1.0);

        label.tick(1.0);
        assert!((label.remaining() - 1.0).abs() < 1e-4);
        assert!(label.alpha() < 1.0 && label.alpha() > 0.0);

        // Same selection is not a change and must not restart the fade.
        assert!(!label.observe(3, Some(stone(5))));
        assert!((label.remaining() - 1.0).abs() < 1e-4);

        // Slot change restarts.
        assert!(label.observe(4, Some(stone(5))));
        assert_eq!(label.remaining(), SELECTED_NAME_SECS);

        // Count change on the same slot restarts (stack identity differs).
        label.tick(1.5);
        assert!(label.observe(4, Some(stone(2))));
        assert_eq!(label.remaining(), SELECTED_NAME_SECS);

        // Durability change restarts too.
        label.tick(1.0);
        assert!(label.observe(4, Some(pickaxe(100))));
        assert_eq!(label.remaining(), SELECTED_NAME_SECS);
        assert!(label.observe(4, Some(pickaxe(99))));
        assert_eq!(label.remaining(), SELECTED_NAME_SECS);
    }

    #[test]
    fn fades_to_invisible_after_the_full_duration() {
        let mut label = SelectionLabel::new();
        label.observe(1, Some(stone(1)));
        label.tick(1.0);
        assert!(label.is_visible(), "halfway through is still visible");
        label.tick(1.0);
        assert_eq!(label.remaining(), 0.0);
        assert!(!label.is_visible());
        assert_eq!(label.label(), None);
        assert_eq!(label.alpha(), 0.0);

        // Overshooting clamps instead of going negative.
        label.tick(10.0);
        assert_eq!(label.remaining(), 0.0);
        assert_eq!(label.alpha(), 0.0);
    }

    #[test]
    fn empty_selection_never_shows_a_label() {
        let mut label = SelectionLabel::new();
        assert!(label.observe(2, None));
        assert!(!label.is_visible());
        assert_eq!(label.label(), None);

        // Moving from an occupied slot to an empty one hides immediately even
        // though the timer is restarted.
        label.observe(2, Some(stone(4)));
        assert!(label.is_visible());
        assert!(label.observe(3, None));
        assert!(!label.is_visible());
        assert_eq!(label.label(), None);
    }

    #[test]
    fn non_finite_and_non_positive_ticks_are_ignored() {
        let mut label = SelectionLabel::new();
        label.observe(0, Some(stone(3)));
        label.tick(1.0);
        let before = label.remaining();
        for dt in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -1.0] {
            label.tick(dt);
            assert!(
                (label.remaining() - before).abs() < 1e-6,
                "dt {dt} must not advance the timer"
            );
        }
        assert!(label.is_visible());
    }

    #[test]
    fn label_text_matches_selected_item_label_exactly() {
        let mut label = SelectionLabel::new();
        label.observe(0, Some(stone(3)));
        assert_eq!(
            label.label(),
            Some(crate::ui::hud::selected_item_label(Some(stone(3)))),
            "block count text"
        );
        assert_eq!(label.label().as_deref(), Some("Stone x3"));

        label.observe(1, Some(pickaxe(137)));
        assert_eq!(label.label().as_deref(), Some("Iron Pickaxe (137/250)"));

        let sticks = Item::new(ItemId::Material(Material::Stick), 1).expect("sticks");
        label.observe(2, Some(sticks));
        assert_eq!(label.label().as_deref(), Some("Stick x1"));
    }
}
