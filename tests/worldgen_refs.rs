//! Checked-in reference prefab proof (M015/S01/T04).
//!
//! These tests read the real `worldgen/prefabs/*.toml` tree from the crate
//! root, so a hand edit that drifts from the slice contract fails CI. They
//! also pin the exact `--validate-worldgen` report for the reference content,
//! which is the slice demo.

use std::collections::HashSet;
use std::path::Path;

use opencraft::world::block::{BlockId, BUILTIN_BLOCK_COUNT};
use opencraft::world::prefab::{validate_worldgen_lines, Prefab, PrefabCatalog};

/// The three reference prefabs that S02 village definitions and S07 docs
/// consume. Names are the stable content API and must not be renamed here.
struct Reference {
    name: &'static str,
    size: [u32; 3],
    origin: [u32; 3],
    sockets: usize,
}

const REFERENCES: [Reference; 3] = [
    Reference {
        name: "plains_hub",
        size: [7, 4, 7],
        origin: [3, 0, 3],
        sockets: 4,
    },
    Reference {
        name: "small_house",
        size: [5, 4, 5],
        origin: [2, 0, 2],
        sockets: 1,
    },
    Reference {
        name: "wheat_farm",
        size: [7, 2, 7],
        origin: [3, 0, 3],
        sockets: 1,
    },
];

fn worldgen_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("worldgen")
}

fn load_catalog() -> PrefabCatalog {
    let (catalog, diagnostics) = PrefabCatalog::load_sorted(&worldgen_root());
    assert!(diagnostics.is_empty(), "diagnostics: {diagnostics:?}");
    catalog
}

fn block_at(prefab: &Prefab, position: [u32; 3]) -> BlockId {
    prefab
        .blocks
        .iter()
        .find(|block| block.position == position)
        .unwrap_or_else(|| panic!("{} has no block at {position:?}", prefab.name))
        .block
}

#[test]
fn validate_worldgen_report_is_exact_for_reference_tree() {
    let (lines, ok) = validate_worldgen_lines(&worldgen_root());
    assert_eq!(
        lines,
        vec![
            "ok: prefab plains_hub".to_owned(),
            "ok: prefab small_house".to_owned(),
            "ok: prefab wheat_farm".to_owned(),
            "validate-worldgen: 3 prefabs, 0 errors, 0 warnings".to_owned(),
        ]
    );
    assert!(ok);
}

#[test]
fn catalog_loads_three_sorted_reference_prefabs_without_diagnostics() {
    let (catalog, diagnostics) = PrefabCatalog::load_sorted(&worldgen_root());
    assert!(diagnostics.is_empty(), "diagnostics: {diagnostics:?}");
    assert_eq!(catalog.len(), 3);
    assert_eq!(
        catalog.names().collect::<Vec<_>>(),
        vec!["plains_hub", "small_house", "wheat_farm"]
    );
}

#[test]
fn reference_prefabs_match_the_pinned_geometry_contract() {
    let catalog = load_catalog();
    let prefabs_dir = worldgen_root().join("prefabs");

    for reference in REFERENCES {
        let prefab = catalog
            .get(reference.name)
            .unwrap_or_else(|| panic!("missing reference prefab {}", reference.name));

        // Header `name` equals the file stem (the file that carries it).
        assert_eq!(prefab.name, reference.name);
        assert!(
            prefabs_dir.join(format!("{}.toml", reference.name)).is_file(),
            "{} must live in worldgen/prefabs/{}.toml",
            reference.name,
            reference.name
        );

        assert_eq!(prefab.size, reference.size, "{} size", reference.name);
        assert_eq!(
            prefab.origin, reference.origin,
            "{} origin",
            reference.name
        );

        // Every cell of the grid is present (air included), only built-ins are
        // used, and the prefab explicitly carries at least one air cell.
        let expected_blocks =
            (reference.size[0] as usize) * (reference.size[1] as usize) * (reference.size[2] as usize);
        assert_eq!(
            prefab.blocks.len(),
            expected_blocks,
            "{} must contain every grid cell",
            reference.name
        );
        for block in &prefab.blocks {
            assert!(
                block.block.to_raw() < BUILTIN_BLOCK_COUNT,
                "{} uses non-built-in block {:?} at {:?}",
                reference.name,
                block.block,
                block.position
            );
        }
        assert!(
            prefab.blocks.iter().any(|block| block.block == BlockId::Air),
            "{} must contain at least one air block",
            reference.name
        );

        assert_eq!(
            prefab.sockets.len(),
            reference.sockets,
            "{} socket count",
            reference.name
        );
    }
}

#[test]
fn every_socket_is_in_bounds_unique_and_attached_to_air() {
    let catalog = load_catalog();

    for reference in REFERENCES {
        let prefab = catalog
            .get(reference.name)
            .unwrap_or_else(|| panic!("missing reference prefab {}", reference.name));
        let mut seen: HashSet<[u32; 3]> = HashSet::new();

        for socket in &prefab.sockets {
            let mut grid = [0u32; 3];
            for axis in 0..3 {
                let cell = i64::from(prefab.origin[axis]) + i64::from(socket.position[axis]);
                assert!(
                    (0..i64::from(reference.size[axis])).contains(&cell),
                    "{} socket {:?} axis {axis} maps outside the grid",
                    reference.name,
                    socket.position
                );
                grid[axis] = cell as u32;
            }
            assert!(
                seen.insert(grid),
                "{} has a duplicate socket at grid {grid:?}",
                reference.name
            );
            assert_eq!(
                block_at(prefab, grid),
                BlockId::Air,
                "{} socket {:?} must attach to an air grid cell, got {:?}",
                reference.name,
                socket.position,
                block_at(prefab, grid)
            );
        }
    }
}

#[test]
fn small_house_contains_exactly_one_chest() {
    let catalog = load_catalog();
    let prefab = catalog.get("small_house").expect("small_house prefab");
    assert_eq!(
        prefab
            .blocks
            .iter()
            .filter(|block| block.block == BlockId::Chest)
            .count(),
        1
    );
}

#[test]
fn wheat_farm_contains_farmland_and_stage_zero_crops_only() {
    let catalog = load_catalog();
    let prefab = catalog.get("wheat_farm").expect("wheat_farm prefab");

    assert!(prefab
        .blocks
        .iter()
        .any(|block| block.block == BlockId::Farmland));
    assert!(prefab
        .blocks
        .iter()
        .any(|block| block.block == BlockId::WheatStage0));

    for stage in [
        BlockId::WheatStage1,
        BlockId::WheatStage2,
        BlockId::WheatStage3,
    ] {
        assert!(
            !prefab.blocks.iter().any(|block| block.block == stage),
            "wheat_farm must not contain {stage:?}"
        );
    }
}
