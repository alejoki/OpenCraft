//! Authoritative survival vocabulary: game modes and the vitals triple.
//!
//! M008 introduces exactly one mode/vitals vocabulary so persistence, session
//! restore, the UI, and later survival mechanics (fall damage, drowning, food)
//! all read and write the same types. The rules are deliberately fail-closed:
//!
//! * A [`SurvivalState`] can only exist through [`SurvivalState::new`] (checked)
//!   or [`SurvivalState::full`] (the all-max baseline), so an out-of-range value
//!   can never be constructed even by future decode paths.
//! * `MAX_AIR` (1200) is the wire ceiling used by the save codec, while
//!   `FULL_AIR` (300) is the gameplay refill value. The two are intentionally
//!   distinct: a save may legally carry more than a full lung (for example a
//!   future potion effect) but never more than the codec ceiling.
//! * Flight is a *mode* capability, not a player flag: `GameMode::allows_fly` is
//!   the only source of truth, and `Player::can_fly` structurally ANDs it with
//!   the toggle so Survival flight cannot be reached by assigning `fly_mode`.

/// Maximum health points. Zero health is a valid state (death screen, S04).
pub const MAX_HEALTH: u16 = 20;

/// Maximum hunger points. Zero hunger is valid (starving, hand-swing/eat rules).
pub const MAX_HUNGER: u16 = 20;

/// Air value a full lung refills to during gameplay.
pub const FULL_AIR: u16 = 300;

/// Fail-closed wire ceiling for air; decode rejects anything above this.
pub const MAX_AIR: u16 = 1200;

/// The two supported game modes.
///
/// `Player::new` starts every fresh player in [`GameMode::Creative`] so all
/// pre-M008 CLI demos, smokes, and physics tests keep their flying default; the
/// New Game row and the v6 save decide what a *restored* world actually uses.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum GameMode {
    Survival,
    Creative,
}

impl GameMode {
    /// Whether this mode permits flight at all. The player still has to have
    /// its fly toggle on; see `Player::can_fly`.
    pub const fn allows_fly(self) -> bool {
        matches!(self, Self::Creative)
    }

    /// Stable human-readable label, also the persisted UI name.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Survival => "Survival",
            Self::Creative => "Creative",
        }
    }

    /// Cycles to the other mode (the New Game mode row uses this).
    pub const fn next(self) -> Self {
        match self {
            Self::Survival => Self::Creative,
            Self::Creative => Self::Survival,
        }
    }
}

/// The persisted vitals triple: health, hunger, and breath.
///
/// Fields are private and the only constructors are checked, so every existing
/// value is within `0..=MAX_HEALTH`, `0..=MAX_HUNGER`, and `0..=MAX_AIR`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SurvivalState {
    health: u16,
    hunger: u16,
    air: u16,
}

impl SurvivalState {
    /// The all-max baseline used by fresh players, v5 migration, and Creative.
    pub const fn full() -> Self {
        Self {
            health: MAX_HEALTH,
            hunger: MAX_HUNGER,
            air: FULL_AIR,
        }
    }

    /// Checked constructor. Returns `None` if any field exceeds its ceiling.
    ///
    /// Zero is accepted for health and hunger (dead / starving states are
    /// reachable gameplay, S04 owns the death screen); air above `MAX_AIR` is a
    /// malformed record rather than gameplay, so it is rejected fail-closed.
    pub const fn new(health: u16, hunger: u16, air: u16) -> Option<Self> {
        if health <= MAX_HEALTH && hunger <= MAX_HUNGER && air <= MAX_AIR {
            Some(Self {
                health,
                hunger,
                air,
            })
        } else {
            None
        }
    }

    pub const fn health(&self) -> u16 {
        self.health
    }

    pub const fn hunger(&self) -> u16 {
        self.hunger
    }

    pub const fn air(&self) -> u16 {
        self.air
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_is_max_health_hunger_and_full_lung() {
        let full = SurvivalState::full();
        assert_eq!(full.health(), MAX_HEALTH);
        assert_eq!(full.hunger(), MAX_HUNGER);
        assert_eq!(full.air(), FULL_AIR);
        // FULL_AIR is a gameplay value, not the wire ceiling.
        const { assert!(FULL_AIR < MAX_AIR) };
    }

    #[test]
    fn zero_and_max_are_accepted_per_field() {
        assert!(SurvivalState::new(0, 0, 0).is_some());
        assert!(SurvivalState::new(MAX_HEALTH, MAX_HUNGER, MAX_AIR).is_some());
        let checked = SurvivalState::new(MAX_HEALTH, MAX_HUNGER, MAX_AIR).unwrap();
        assert_eq!(checked.health(), MAX_HEALTH);
        assert_eq!(checked.hunger(), MAX_HUNGER);
        assert_eq!(checked.air(), MAX_AIR);
    }

    #[test]
    fn one_over_max_is_rejected_per_field() {
        assert!(SurvivalState::new(MAX_HEALTH + 1, MAX_HUNGER, MAX_AIR).is_none());
        assert!(SurvivalState::new(MAX_HEALTH, MAX_HUNGER + 1, MAX_AIR).is_none());
        assert!(SurvivalState::new(MAX_HEALTH, MAX_HUNGER, MAX_AIR + 1).is_none());
        // u16::MAX is the extreme malformed wire value and must also fail.
        assert!(SurvivalState::new(u16::MAX, 0, 0).is_none());
        assert!(SurvivalState::new(0, u16::MAX, 0).is_none());
        assert!(SurvivalState::new(0, 0, u16::MAX).is_none());
    }

    #[test]
    fn mode_labels_are_stable() {
        assert_eq!(GameMode::Survival.label(), "Survival");
        assert_eq!(GameMode::Creative.label(), "Creative");
    }

    #[test]
    fn only_creative_allows_flight() {
        assert!(GameMode::Creative.allows_fly());
        assert!(!GameMode::Survival.allows_fly());
    }

    #[test]
    fn next_cycles_between_the_two_modes() {
        assert_eq!(GameMode::Survival.next(), GameMode::Creative);
        assert_eq!(GameMode::Creative.next(), GameMode::Survival);
        assert_eq!(GameMode::Survival.next().next(), GameMode::Survival);
    }
}
