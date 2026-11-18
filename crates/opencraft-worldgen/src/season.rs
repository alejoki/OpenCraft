//! Neutral season vocabulary for seasonal surface/leaf policy.
//!
//! Deliberately a mirror of the game `Season` variant set. The game owns the
//! calendar (`season_of`, day constants, weather) and maps its own enum onto
//! this one at the adapter boundary; this crate only needs the four variants to
//! paint the seasonal land skin and decide whether a tree keeps its canopy.

/// Calendar season a chunk is generated for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Season {
    Spring,
    Summer,
    Autumn,
    Winter,
}

impl Season {
    /// Whether this season paints the snow blanket and sheds the oak canopy.
    pub const fn is_winter(self) -> bool {
        matches!(self, Season::Winter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_seasons_are_distinct_and_winter_is_the_only_snow_season() {
        let all = [
            Season::Spring,
            Season::Summer,
            Season::Autumn,
            Season::Winter,
        ];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                assert_eq!(i == j, a == b, "{a:?} and {b:?} must be distinct");
            }
        }
        assert!(!Season::Spring.is_winter());
        assert!(!Season::Summer.is_winter());
        assert!(!Season::Autumn.is_winter());
        assert!(Season::Winter.is_winter());
    }
}
