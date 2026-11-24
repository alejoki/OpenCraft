//! Game-side shim over the neutral prefab codec in `opencraft-worldgen`.
//!
//! The parser, canonical serializer, identity scan, fail-soft catalog and the
//! `--validate-worldgen` report all live in [`opencraft_worldgen::prefab`];
//! this module re-exports them unchanged so every `crate::world::prefab::...`
//! path keeps working. The one visible difference is `PrefabBlock.block`, a
//! raw `u16` block id: game call sites convert with
//! [`BlockId::to_raw`](crate::world::block::BlockId::to_raw) /
//! [`BlockId::from_raw`](crate::world::block::BlockId::from_raw) at the
//! boundary.
//!
//! Capture ([`crate::world::capture`]) deliberately stays game-side — it is
//! coupled to [`WorldManager`](crate::world::manager::WorldManager) and its
//! `CaptureError` carries a [`BlockId`](crate::world::block::BlockId).

pub use opencraft_worldgen::prefab::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::block::BlockId;

    const STONE_PREFAB: &str = r###"format = 1
name = "stone_cell"
origin = [0, 0, 0]
size = [2, 1, 1]

layers = [
  [ "##" ],
]

[palette]
"#" = "stone"
"###;

    #[test]
    fn parse_prefab_resolves_builtin_names_to_raw_ids() {
        let prefab = parse_prefab(STONE_PREFAB).expect("stone prefab parses");
        assert_eq!(prefab.blocks.len(), 2);
        for block in &prefab.blocks {
            assert_eq!(BlockId::from_raw(block.block), BlockId::Stone);
        }
    }

    #[test]
    fn to_toml_string_writes_the_builtin_name_back() {
        let prefab = parse_prefab(STONE_PREFAB).expect("stone prefab parses");
        let text = prefab.to_toml_string();
        assert!(
            text.contains("= \"stone\""),
            "the serializer must write the built-in name, not a raw id:\n{text}"
        );
        assert_eq!(parse_prefab(&text), Ok(prefab));
    }

    #[test]
    fn parse_prefab_exposes_markers_through_the_game_reexport() {
        let text = format!(
            "{STONE_PREFAB}\n[[markers]]\nkind = \"loot_chest\"\ntable = \"ruin\"\nposition = [1, 0, 0]\n"
        );
        let prefab = parse_prefab(&text).expect("marker prefab parses");
        assert_eq!(prefab.markers.len(), 1);
        assert_eq!(prefab.markers[0].position, [1, 0, 0]);
        assert_eq!(prefab.markers[0].kind, MarkerKind::LootChest);
        assert_eq!(prefab.markers[0].table, MarkerLootTable::Ruin);
        assert_eq!(prefab.blocks.len(), 2, "markers stay out of the grid");
        assert_eq!(parse_prefab(&prefab.to_toml_string()), Ok(prefab));
    }
}
