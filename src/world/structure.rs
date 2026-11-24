//! Game-side shim over the neutral structure catalog in `opencraft-worldgen`.
//!
//! Parsing, the compile-time builtin master, the fail-soft runtime overlay and
//! the family validator all live in [`opencraft_worldgen::structure`]; this
//! module re-exports them unchanged so every `crate::world::structure::...`
//! path works. The game keeps only wiring: startup loads a catalog, the
//! manager carries it into generation, and the session installs it at the
//! create/load boundary. Generation itself never reads the filesystem.
//!
//! The same rule as [`crate::world::prefab`]: the neutral crate owns the codec
//! and catalog, the game owns the plumbing.

pub use opencraft_worldgen::structure::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_master_exposes_every_oak_and_ruin_prefab() {
        let catalog = StructureCatalog::builtin();
        assert_eq!(catalog.len(), 11);
        assert!(catalog.missing_required().is_empty());
        assert!(catalog.oak(4, 0).is_some());
        assert!(catalog.oak(5, 3).is_some());
    }

    #[test]
    fn builtin_ruin_lookup_returns_the_named_wall_height() {
        let catalog = StructureCatalog::builtin();
        let ruin = catalog.ruin(2).expect("wall height 2 is authored");
        assert_eq!(ruin.name, "ruin_wall_2");
    }
}
