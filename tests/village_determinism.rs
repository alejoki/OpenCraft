//! Library-level proof that the village subsystem is a pure function of
//! `(cell, seed, catalog)` and that an absent/empty catalog reproduces the
//! pre-M015 generator byte for byte (M015/S02-T06).
//!
//! These tests call the public production seams only — `VillageCatalog`,
//! `village_plan`/`village_plan_with_spawn` and
//! `generate_chunk_with_context` — so they pin the same boundaries the
//! release-facing `--demo-village` digest exercises, without spawning a binary.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use opencraft::world::chunk::{
    local_coords_from_index, Chunk, ChunkPos, CHUNK_SIZE, CHUNK_VOLUME,
};
use opencraft::world::season::Season;
use opencraft::world::terrain::{
    find_spawn_column, generate_chunk_seeded_with, generate_chunk_with_context, WorldGenContext,
};
use opencraft::world::village::{
    village_plan, village_plan_with_spawn, VillageBounds, VillageCatalog, VillageCell, VillagePlan,
};

/// Pinned digest seed shared with `src/main.rs`'s `--demo-village`.
const PINNED_SEED: u32 = 424_242;

/// The first in-band cell the pinned window scan finds for the checked-in tree.
///
/// Measured from a real run (`--demo-village` prints `cell=6,-6`); the test
/// re-runs the same scan and asserts this constant instead of deriving it.
const PINNED_SITE: (i32, i32) = (6, -6);

/// Inclusive authored placement band a demo village must satisfy.
const MIN_PREFABS: usize = 10;
const MAX_PREFABS: usize = 20;

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn worldgen_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("worldgen")
}

/// Load the checked-in tree, asserting the reference counts/diagnostics.
fn load() -> VillageCatalog {
    let (catalog, diagnostics) = VillageCatalog::load_sorted(&worldgen_root());
    assert!(
        diagnostics.is_empty(),
        "checked-in worldgen tree must load without diagnostics: {diagnostics:?}"
    );
    assert_eq!(catalog.prefabs().len(), 3, "three reference prefabs");
    assert_eq!(catalog.len(), 1, "one reference village definition");
    catalog
}

/// Scan the pinned cell window exactly like the demo and return the first plan
/// inside the 10..=20 placement band.
fn find_site<'a>(catalog: &'a VillageCatalog) -> VillagePlan<'a> {
    let spawn = find_spawn_column(PINNED_SEED);
    for cz in -6..=6 {
        for cx in -6..=6 {
            let cell = VillageCell { x: cx, z: cz };
            if let Some(plan) = village_plan_with_spawn(cell, PINNED_SEED, catalog, spawn) {
                if (MIN_PREFABS..=MAX_PREFABS).contains(&plan.placement_count()) {
                    assert_eq!(
                        (plan.cell.x, plan.cell.z),
                        PINNED_SITE,
                        "the pinned window scan must find the measured site"
                    );
                    return plan;
                }
            }
        }
    }
    panic!("the checked-in village tree must expose an in-band site");
}

/// FNV-1a 64 over a chunk's uniform marker (if any) then every raw block id in
/// `linear_index` order.
fn chunk_fingerprint(chunk: &Chunk) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    let uniform = match chunk.is_uniform() {
        Some(block) => u64::from(block.to_raw()) + 1,
        None => 0,
    };
    hash = fnv1a(hash, &uniform.to_le_bytes());
    for index in 0..CHUNK_VOLUME {
        let (x, z, y) = local_coords_from_index(index as u16);
        hash = fnv1a(hash, &chunk.get(x, y, z).to_raw().to_le_bytes());
    }
    hash
}

fn chunks_equal(left: &Chunk, right: &Chunk) -> bool {
    (0..CHUNK_SIZE).all(|y| {
        (0..CHUNK_SIZE)
            .all(|z| (0..CHUNK_SIZE).all(|x| left.get(x, y, z) == right.get(x, y, z)))
    })
}

/// The plan's chunk AABB expanded by one chunk in x/z, over every y layer the
/// bounds touch — identical to the demo's window.
fn window(bounds: &VillageBounds) -> Vec<ChunkPos> {
    let size = CHUNK_SIZE as i32;
    let cx_min = bounds.min_x.div_euclid(size) - 1;
    let cx_max = bounds.max_x.div_euclid(size) + 1;
    let cz_min = bounds.min_z.div_euclid(size) - 1;
    let cz_max = bounds.max_z.div_euclid(size) + 1;
    let cy_min = bounds.min_y.div_euclid(size);
    let cy_max = bounds.max_y.div_euclid(size);
    let mut positions = Vec::new();
    for cz in cz_min..=cz_max {
        for cx in cx_min..=cx_max {
            for cy in cy_min..=cy_max {
                positions.push(ChunkPos::new(cx, cy, cz));
            }
        }
    }
    positions
}

/// Fixed integer permutation key for the shuffled-order leg. Never a
/// correctness input — only a deterministic reordering of the window.
fn order_hash(pos: ChunkPos) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for value in [pos.x, pos.y, pos.z] {
        hash = fnv1a(hash, &(value as i64).to_le_bytes());
    }
    hash
}

fn generate(pos: ChunkPos, catalog: &VillageCatalog, spawn: (i32, i32)) -> Chunk {
    generate_chunk_with_context(
        pos,
        PINNED_SEED,
        Season::Summer,
        &WorldGenContext {
            rules: &[],
            villages: Some(catalog),
            spawn_column: Some(spawn),
        },
    )
}

fn copy_dir_all(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap_or_else(|error| panic!("create {}: {error}", to.display()));
    for entry in std::fs::read_dir(from)
        .unwrap_or_else(|error| panic!("read_dir {}: {error}", from.display()))
    {
        let entry = entry.expect("directory entry");
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_dir_all(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap_or_else(|error| {
                panic!("copy {} -> {}: {error}", path.display(), target.display())
            });
        }
    }
}

fn write_prefab(root: &Path, file_name: &str, text: &str) {
    let dir = root.join("prefabs");
    std::fs::create_dir_all(&dir).expect("create prefab fixture dir");
    std::fs::write(dir.join(file_name), text).expect("write prefab fixture");
}

fn write_village(root: &Path, file_name: &str, text: &str) {
    let dir = root.join("villages");
    std::fs::create_dir_all(&dir).expect("create village fixture dir");
    std::fs::write(dir.join(file_name), text).expect("write village fixture");
}

const HUB: &str = r#"format = 1
name = "hub"
origin = [0, 0, 0]
size = [3, 3, 3]

layers = [
  [ "xxx", "xxx", "xxx" ],
  [ "xxx", "xxx", "xxx" ],
  [ "xxx", "xxx", "xxx" ],
]

[palette]
"x" = "stone"

[[sockets]]
position = [1, 1, 0]
direction = "north"
"#;

const LEAF: &str = r#"format = 1
name = "leaf"
origin = [0, 0, 0]
size = [3, 3, 3]

layers = [
  [ "xxx", "xxx", "xxx" ],
  [ "xxx", "xxx", "xxx" ],
  [ "xxx", "xxx", "xxx" ],
]

[palette]
"x" = "oak_planks"

[[sockets]]
position = [1, 1, 0]
direction = "north"
"#;

fn definition(spacing: i32, pool: &str) -> String {
    format!(
        "format = 1\nname = \"subset\"\nbiomes = [\"plains\", \"forest\"]\nhub = \"hub\"\n\
         spacing = {spacing}\nmin_prefabs = 10\nmax_prefabs = 20\nmax_slope = 16\n\n[pool]\n{pool}"
    )
}

/// Two independent loads of the same tree must produce equal plans for the same
/// `(cell, seed)` — no interior mutability, caches or ordering leaks.
#[test]
fn plan_is_pure() {
    let catalog_a = load();
    let catalog_b = load();
    let plan_a = find_site(&catalog_a);
    let plan_b = find_site(&catalog_b);
    assert_eq!(plan_a, plan_b, "two loads must plan identically");
    assert_eq!(
        plan_a.catalog_fingerprint, plan_b.catalog_fingerprint,
        "equal catalogs must share a fingerprint"
    );
}

/// Row-major, reverse, integer-hash-shuffled and one-chunk-at-a-time generation
/// must all agree chunk by chunk: the village pass holds no cross-chunk state.
#[test]
fn chunk_order_single_and_region_agree() {
    let catalog = load();
    let plan = find_site(&catalog);
    let spawn = find_spawn_column(PINNED_SEED);
    let positions = window(&plan.bounds);

    let baseline: BTreeMap<(i32, i32, i32), u64> = positions
        .iter()
        .map(|&pos| ((pos.x, pos.y, pos.z), chunk_fingerprint(&generate(pos, &catalog, spawn))))
        .collect();

    let mut reversed = positions.clone();
    reversed.reverse();
    let mut shuffled = positions.clone();
    shuffled.sort_by_key(|pos| (order_hash(*pos), pos.y, pos.x, pos.z));

    for (label, ordered) in [("reverse", &reversed), ("shuffled", &shuffled)] {
        for &pos in ordered {
            let found = baseline.get(&(pos.x, pos.y, pos.z)).copied();
            assert_eq!(
                chunk_fingerprint(&generate(pos, &catalog, spawn)),
                found.expect("baseline covers every window chunk"),
                "{label} order drifted at {pos:?}"
            );
        }
    }

    // One chunk at a time: a fresh call per chunk, no region context carried.
    for &pos in &positions {
        let alone = generate(pos, &catalog, spawn);
        assert_eq!(
            chunk_fingerprint(&alone),
            baseline[&(pos.x, pos.y, pos.z)],
            "single-chunk generation drifted at {pos:?}"
        );
    }
}

/// Chunks on the one-chunk border of the plan's chunk AABB hold no village cell
/// and must stay byte-identical to the legacy generator.
#[test]
fn outside_bounds_matches_legacy() {
    let catalog = load();
    let plan = find_site(&catalog);
    let spawn = find_spawn_column(PINNED_SEED);
    let size = CHUNK_SIZE as i32;
    let cx_min = plan.bounds.min_x.div_euclid(size);
    let cx_max = plan.bounds.max_x.div_euclid(size);
    let cz_min = plan.bounds.min_z.div_euclid(size);
    let cz_max = plan.bounds.max_z.div_euclid(size);

    let mut checked = 0usize;
    for pos in window(&plan.bounds) {
        if pos.x >= cx_min && pos.x <= cx_max && pos.z >= cz_min && pos.z <= cz_max {
            continue;
        }
        let legacy = generate_chunk_seeded_with(pos, PINNED_SEED, Season::Summer, &[]);
        let with_village = generate(pos, &catalog, spawn);
        assert_eq!(
            chunk_fingerprint(&with_village),
            chunk_fingerprint(&legacy),
            "outside-border chunk {pos:?} must not change"
        );
        checked += 1;
    }
    assert!(checked > 0, "the window must contain outside-border chunks");
}

/// An absent catalog and an empty catalog must both reproduce
/// `generate_chunk_seeded_with(..., &[])` exactly, over the village window and
/// two seasons (including the seeded/uniform-Air fast paths).
#[test]
fn empty_catalog_is_byte_identical() {
    let catalog = load();
    let plan = find_site(&catalog);
    let empty = VillageCatalog::default();
    let positions = window(&plan.bounds);

    for season in [Season::Summer, Season::Winter] {
        for &pos in &positions {
            let legacy = generate_chunk_seeded_with(pos, PINNED_SEED, season, &[]);
            let via_none = generate_chunk_with_context(
                pos,
                PINNED_SEED,
                season,
                &WorldGenContext {
                    rules: &[],
                    villages: None,
                    spawn_column: None,
                },
            );
            let via_empty = generate_chunk_with_context(
                pos,
                PINNED_SEED,
                season,
                &WorldGenContext {
                    rules: &[],
                    villages: Some(&empty),
                    spawn_column: None,
                },
            );
            assert!(
                chunks_equal(&legacy, &via_none),
                "villages: None drifted at {pos:?}, {season:?}"
            );
            assert!(
                chunks_equal(&legacy, &via_empty),
                "empty catalog drifted at {pos:?}, {season:?}"
            );
        }
    }
}

/// The checked-in catalog fingerprint is stable across loads.
#[test]
fn checked_in_catalog_fingerprint_is_stable() {
    let first = load();
    let second = load();
    assert_eq!(first.fingerprint(), second.fingerprint());
    assert_ne!(first.fingerprint(), VillageCatalog::default().fingerprint());
}

/// Changing one authored canonical field moves the fingerprint, so the digest is
/// sensitive to content rather than to file bytes or whitespace.
#[test]
fn fingerprint_moves_when_definition_changes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("worldgen");
    copy_dir_all(&worldgen_root(), &root);

    let (before, diagnostics) = VillageCatalog::load_sorted(&root);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    let path = root.join("villages/plains.toml");
    let text = std::fs::read_to_string(&path).expect("read plains.toml");
    assert_eq!(
        text.matches("spacing = 4").count(),
        1,
        "fixture edit must land exactly once"
    );
    std::fs::write(&path, text.replacen("spacing = 4", "spacing = 5", 1))
        .expect("write edited plains.toml");

    let (after, diagnostics) = VillageCatalog::load_sorted(&root);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_ne!(
        before.fingerprint(),
        after.fingerprint(),
        "a canonical field change must move the fingerprint"
    );
}

/// Every loader failure path degrades to a diagnostic or empty content, never a
/// panic: invalid TOML, duplicate names, an unknown pool prefab and a missing
/// root.
#[test]
fn loader_is_fail_soft() {
    // (a) Invalid village TOML: one diagnostic, no accepted definition.
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("worldgen");
    write_village(&root, "broken.toml", "format = 1\n");
    let (catalog, diagnostics) = VillageCatalog::load_sorted(&root);
    assert_eq!(
        diagnostics,
        vec!["worldgen/villages/broken.toml: missing name".to_owned()]
    );
    assert!(catalog.is_empty());

    // (b) Duplicate definition names: the sorted-first file wins and the later
    // one reports exactly one duplicate diagnostic (spacing distinguishes them).
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("worldgen");
    write_prefab(&root, "hub.toml", HUB);
    write_prefab(&root, "leaf.toml", LEAF);
    write_village(
        &root,
        "a.toml",
        &definition(4, "house = [\"leaf\"]\njunction = [\"hub\"]\n"),
    );
    write_village(
        &root,
        "b.toml",
        &definition(5, "house = [\"leaf\"]\njunction = [\"hub\"]\n"),
    );
    let (catalog, diagnostics) = VillageCatalog::load_sorted(&root);
    assert_eq!(
        diagnostics,
        vec!["worldgen/villages/b.toml: duplicate village name 'subset'".to_owned()]
    );
    let kept = catalog.get("subset").expect("sorted-first definition survives");
    assert_eq!(kept.spacing, 4, "the sorted-first file must win");

    // (c) Unknown pool prefab: dropped with one diagnostic, definition survives
    // because another leaf remains.
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("worldgen");
    write_prefab(&root, "hub.toml", HUB);
    write_prefab(&root, "leaf.toml", LEAF);
    write_village(
        &root,
        "subset.toml",
        &definition(
            4,
            "house = [\"leaf\", \"ghost\"]\njunction = [\"hub\"]\n",
        ),
    );
    let (catalog, diagnostics) = VillageCatalog::load_sorted(&root);
    assert_eq!(
        diagnostics,
        vec![
            "worldgen/villages/subset.toml: pool 'house' names unknown prefab 'ghost'".to_owned()
        ]
    );
    assert!(catalog.get("subset").is_some(), "definition must survive");

    // (d) A missing root is valid empty content with no diagnostics.
    let temp = tempfile::tempdir().expect("tempdir");
    let (catalog, diagnostics) = VillageCatalog::load_sorted(&temp.path().join("absent"));
    assert!(catalog.is_empty());
    assert!(diagnostics.is_empty());
    assert_eq!(catalog.prefabs().len(), 0);
}

/// A site whose hub fits but whose authored band cannot be reached still
/// returns a plan with the subset of placements that fit — never a rejection.
#[test]
fn subset_plan_is_returned() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("worldgen");
    write_prefab(&root, "hub.toml", HUB);
    write_prefab(&root, "leaf.toml", LEAF);
    write_village(
        &root,
        "subset.toml",
        &definition(4, "house = [\"leaf\"]\n"),
    );
    let (catalog, diagnostics) = VillageCatalog::load_sorted(&root);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    let spawn = find_spawn_column(PINNED_SEED);
    let mut found = None;
    'scan: for cz in -6..=6 {
        for cx in -6..=6 {
            let cell = VillageCell { x: cx, z: cz };
            if let Some(plan) = village_plan_with_spawn(cell, PINNED_SEED, &catalog, spawn) {
                found = Some(plan);
                break 'scan;
            }
        }
    }
    let plan = found.expect("a minimal hub/leaf definition must fit some site");
    assert!(
        (1..=9).contains(&plan.placement_count()),
        "subset policy must return a partial plan, got {} placements",
        plan.placement_count()
    );
    assert_eq!(plan.definition, "subset");
}

/// `village_plan` (spawn search inside) and `village_plan_with_spawn` (spawn
/// supplied) are the same plan for the pinned site.
#[test]
fn spawn_aware_and_default_plan_agree() {
    let catalog = load();
    let cell = VillageCell {
        x: PINNED_SITE.0,
        z: PINNED_SITE.1,
    };
    let with_spawn = village_plan_with_spawn(
        cell,
        PINNED_SEED,
        &catalog,
        find_spawn_column(PINNED_SEED),
    );
    let default_plan = village_plan(cell, PINNED_SEED, &catalog);
    assert_eq!(with_spawn, default_plan);
    assert!(default_plan.is_some());
}

