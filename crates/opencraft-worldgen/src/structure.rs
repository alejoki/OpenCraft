//! Authored structure prefabs: the compile-time builtin master for oaks and
//! surface ruins, plus a runtime overlay by name.
//!
//! Structures are a **separate content family** from [`crate::prefab`]
//! villages (D348): they live in `worldgen/structures/`, they are scanned by
//! [`StructureCatalog`] rather than [`crate::prefab::PrefabCatalog`], and they
//! are *never* consulted by village definitions. That separation is what keeps
//! `VillageCatalog::fingerprint()` and the `3 prefabs` village pins unchanged
//! when structure content is edited.
//!
//! The tracked TOML files are both the authoring surface and the **builtin
//! master**: [`StructureCatalog::builtin`] `include_str!`s all eleven documents
//! and parses them once into a [`OnceLock`]. Generation therefore never needs a
//! filesystem (`generate_chunk_seeded`, `engine-reuse-proof` and every headless
//! test keep working with no repository content), while an author editing
//! `worldgen/structures/` gets that edit through
//! [`StructureCatalog::load_sorted`]. A rejected runtime file degrades to
//! exactly one diagnostic and leaves the builtin of that name in place, so a
//! typo can never silently delete trees or ruins.
//!
//! Placement math stays in code (D349): a prefab supplies only cells and typed
//! markers. Grid `y = 0` is the **first cell above the column surface**, so a
//! block or marker at `position` binds at `h_column + 1 + position.y`. The ruin
//! family validator pins `size.y == wall_height + 1` and `origin == [2, 0, 2]`,
//! which is exactly the legacy shell plus the one-cell headroom the corner
//! pillars need.
//!
//! Two failure policies, identical to the prefab family and never mixed:
//!
//! * [`StructureCatalog::load_sorted`] is **fail-soft** — one diagnostic per
//!   rejected file, builtin fallback, world load always continues.
//! * [`validate_worldgen_lines`] is **fail-closed** — it renders the combined
//!   prefab + structure report (D350) and returns `ok == false` on any rejected
//!   file or missing required name.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::OnceLock;

use crate::block::{
    BLOCK_AIR, BLOCK_BRICK, BLOCK_COBBLESTONE, BLOCK_LOOT_CHEST, BLOCK_OAK_LEAVES, BLOCK_OAK_LOG,
};
use crate::prefab::{
    logical_file_label_in, parse_prefab, prefab_section, scan_prefab_dir_in, MarkerKind,
    MarkerLootTable, Prefab,
};

/// Directory, below [`crate::prefab::WORLDGEN_DIR`], that owns one TOML file
/// per authored structure prefab.
pub const STRUCTURES_SUBDIR: &str = "structures";

/// The eight oak variants: trunk height 4 and 5, branch layouts 0..=3.
pub const OAK_PREFABS: [&str; 8] = [
    "oak_t4_l0",
    "oak_t4_l1",
    "oak_t4_l2",
    "oak_t4_l3",
    "oak_t5_l0",
    "oak_t5_l1",
    "oak_t5_l2",
    "oak_t5_l3",
];

/// The three ruin wall heights the placement rule can emit.
pub const RUIN_PREFABS: [&str; 3] = ["ruin_wall_1", "ruin_wall_2", "ruin_wall_3"];

/// Size every oak prefab must declare: canopy radius 2 around a 5-wide column
/// footprint, and one spare layer above the tallest emitted cell.
const OAK_SIZE: [u32; 3] = [5, 6, 5];

/// Origin every structure prefab must declare: the centre of the footprint.
///
/// Grid `x/z` extent is `0..=4` around the origin column, so every cell stays
/// chunk-interior for the placement rule's `3..29` margin.
const STRUCTURE_ORIGIN: [u32; 3] = [2, 0, 2];

/// Branch-cell count per oak layout; the model's `[2, 3, 4, 3]` rotation.
const OAK_BRANCH_COUNTS: [usize; 4] = [2, 3, 4, 3];

/// The checked-in master documents, compiled into the crate.
///
/// These are the same files an author edits at runtime; a drift test in this
/// module compares the parsed builtin to the on-disk tree.
const MASTER_FILES: [(&str, &str); 11] = [
    (
        "oak_t4_l0",
        include_str!("../../../worldgen/structures/oak_t4_l0.toml"),
    ),
    (
        "oak_t4_l1",
        include_str!("../../../worldgen/structures/oak_t4_l1.toml"),
    ),
    (
        "oak_t4_l2",
        include_str!("../../../worldgen/structures/oak_t4_l2.toml"),
    ),
    (
        "oak_t4_l3",
        include_str!("../../../worldgen/structures/oak_t4_l3.toml"),
    ),
    (
        "oak_t5_l0",
        include_str!("../../../worldgen/structures/oak_t5_l0.toml"),
    ),
    (
        "oak_t5_l1",
        include_str!("../../../worldgen/structures/oak_t5_l1.toml"),
    ),
    (
        "oak_t5_l2",
        include_str!("../../../worldgen/structures/oak_t5_l2.toml"),
    ),
    (
        "oak_t5_l3",
        include_str!("../../../worldgen/structures/oak_t5_l3.toml"),
    ),
    (
        "ruin_wall_1",
        include_str!("../../../worldgen/structures/ruin_wall_1.toml"),
    ),
    (
        "ruin_wall_2",
        include_str!("../../../worldgen/structures/ruin_wall_2.toml"),
    ),
    (
        "ruin_wall_3",
        include_str!("../../../worldgen/structures/ruin_wall_3.toml"),
    ),
];

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// A name-sorted collection of authored structure prefabs.
///
/// The builtin master ([`StructureCatalog::builtin`]) is always complete, so
/// `len()` is normally 11 and [`StructureCatalog::missing_required`] is empty.
/// A runtime catalog from [`StructureCatalog::load_sorted`] starts as a builtin
/// clone and overlays accepted files by name; it can therefore never lose a
/// required prefab to a broken file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StructureCatalog {
    prefabs: BTreeMap<String, Prefab>,
}

impl StructureCatalog {
    /// The compile-time builtin master: all eleven tracked documents, parsed
    /// once and shared for the process lifetime.
    ///
    /// No filesystem access happens here. Both parsing and the family
    /// invariants are asserted at first use, so a corrupted master is a loud
    /// development failure rather than a silent generation change.
    pub fn builtin() -> &'static StructureCatalog {
        static BUILTIN: OnceLock<StructureCatalog> = OnceLock::new();
        BUILTIN.get_or_init(|| {
            let prefabs = MASTER_FILES
                .iter()
                .map(|(name, text)| {
                    let prefab = parse_prefab(text).unwrap_or_else(|error| {
                        panic!("builtin structure '{name}' must parse: {error}")
                    });
                    validate_structure_prefab(&prefab).unwrap_or_else(|reason| {
                        panic!("builtin structure '{name}' is invalid: {reason}")
                    });
                    prefab
                })
                .collect();
            StructureCatalog::from_prefabs(prefabs)
        })
    }

    /// Walk `root/structures` and overlay accepted files onto the builtin.
    ///
    /// Fail-soft by contract, exactly like [`crate::prefab::PrefabCatalog`]:
    /// the returned catalog always contains the builtin master, and every
    /// rejected file contributes one `<label>: <reason>` diagnostic while the
    /// builtin of that name keeps generating. Per file the first failure wins:
    ///
    /// 1. the shared walk policy (metadata/read, 1 MiB cap, parse, duplicate
    ///    declared name) from [`scan_prefab_dir_in`];
    /// 2. the file stem must equal the declared prefab name, otherwise
    ///    `file name does not match prefab name '<name>'`;
    /// 3. [`validate_structure_prefab`] must accept the family invariants.
    ///
    /// Unknown extra names are accepted (only the generic loot-chest rule
    /// applies) so future structure kinds do not need a code change to load.
    pub fn load_sorted(root: &Path) -> (Self, Vec<String>) {
        let mut catalog = StructureCatalog::builtin().clone();
        let mut diagnostics = Vec::new();
        for file in scan_prefab_dir_in(root, STRUCTURES_SUBDIR) {
            match file.outcome {
                Ok(prefab) => {
                    let stem = Path::new(&file.file_name)
                        .file_stem()
                        .and_then(OsStr::to_str)
                        .unwrap_or_default();
                    if stem != prefab.name {
                        diagnostics.push(logical_file_label_in(
                            STRUCTURES_SUBDIR,
                            &file.file_name,
                            &format!("file name does not match prefab name '{}'", prefab.name),
                        ));
                        continue;
                    }
                    if let Err(reason) = validate_structure_prefab(&prefab) {
                        diagnostics.push(logical_file_label_in(
                            STRUCTURES_SUBDIR,
                            &file.file_name,
                            &reason,
                        ));
                        continue;
                    }
                    catalog.prefabs.insert(prefab.name.clone(), prefab);
                }
                Err(reason) => diagnostics.push(logical_file_label_in(
                    STRUCTURES_SUBDIR,
                    &file.file_name,
                    &reason,
                )),
            }
        }
        (catalog, diagnostics)
    }

    /// Build a catalog directly from parsed prefabs, later names winning.
    ///
    /// Mirrors [`crate::prefab::PrefabCatalog::from_prefabs`]; used by tests
    /// and by callers that already hold parsed content.
    pub fn from_prefabs(prefabs: Vec<Prefab>) -> Self {
        let mut catalog = StructureCatalog::default();
        for prefab in prefabs {
            catalog.prefabs.insert(prefab.name.clone(), prefab);
        }
        catalog
    }

    /// Look a structure prefab up by its header `name`.
    pub fn get(&self, name: &str) -> Option<&Prefab> {
        self.prefabs.get(name)
    }

    /// Iterate loaded structure names in ascending order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.prefabs.keys().map(String::as_str)
    }

    /// Number of effective structure prefabs.
    pub fn len(&self) -> usize {
        self.prefabs.len()
    }

    /// Whether the catalog holds no prefabs at all.
    pub fn is_empty(&self) -> bool {
        self.prefabs.is_empty()
    }

    /// The oak prefab for `trunk_height` (4 or 5) and `layout` (0..=3).
    ///
    /// Returns `None` for any combination the authored family does not define,
    /// which the placement pass treats as "skip this tree" rather than an
    /// error.
    pub fn oak(&self, trunk_height: i32, layout: u8) -> Option<&Prefab> {
        self.get(&format!("oak_t{trunk_height}_l{layout}"))
    }

    /// The ruin prefab for `wall_height` (1..=3), or `None` when undefined.
    pub fn ruin(&self, wall_height: i32) -> Option<&Prefab> {
        self.get(&format!("ruin_wall_{wall_height}"))
    }

    /// Names from [`OAK_PREFABS`] and [`RUIN_PREFABS`] the catalog lacks.
    pub fn missing_required(&self) -> Vec<&'static str> {
        OAK_PREFABS
            .iter()
            .chain(RUIN_PREFABS.iter())
            .copied()
            .filter(|name| !self.prefabs.contains_key(*name))
            .collect()
    }

    /// Canonical content digest, mirroring `VillageCatalog::fingerprint`.
    ///
    /// FNV-1a 64 over each `name`, a `0` separator and the canonical
    /// [`Prefab::to_toml_string`] bytes in ascending name order, then a final
    /// `0xff` terminator. Whitespace-only edits do not move it; a cell or
    /// marker change does.
    pub fn fingerprint(&self) -> u64 {
        let mut hash = FNV_OFFSET_BASIS;
        for (name, prefab) in &self.prefabs {
            hash = fnv1a(hash, name.as_bytes());
            hash = fnv1a(hash, &[0]);
            hash = fnv1a(hash, prefab.to_toml_string().as_bytes());
        }
        fnv1a(hash, &[0xff])
    }
}

/// Check the family invariants for one parsed structure prefab.
///
/// Exact reason strings are pinned: they appear verbatim in startup warnings
/// and in the `--validate-worldgen` error line. A name outside the oak and ruin
/// families only has to obey the generic rule, which keeps future structure
/// kinds forward compatible while still banning an authored chest block.
fn validate_structure_prefab(prefab: &Prefab) -> Result<(), String> {
    if prefab
        .blocks
        .iter()
        .any(|block| block.block == BLOCK_LOOT_CHEST)
    {
        return Err(
            "structure prefab may not contain a loot_chest block; use a [[markers]] entry"
                .to_owned(),
        );
    }
    if let Some((trunk, layout)) = oak_geometry(&prefab.name) {
        return validate_oak_prefab(prefab, trunk, layout);
    }
    if let Some(wall_height) = ruin_wall_geometry(&prefab.name) {
        return validate_ruin_prefab(prefab, wall_height);
    }
    Ok(())
}

/// `(trunk_height, layout)` for one of the eight authored oak names.
///
/// Exact-match on purpose: a future `oak_t6_l0` is an unknown extra that only
/// gets the generic validation, matching the forward-compatibility policy.
fn oak_geometry(name: &str) -> Option<(i32, u8)> {
    const NAMES: [(&str, i32, u8); 8] = [
        ("oak_t4_l0", 4, 0),
        ("oak_t4_l1", 4, 1),
        ("oak_t4_l2", 4, 2),
        ("oak_t4_l3", 4, 3),
        ("oak_t5_l0", 5, 0),
        ("oak_t5_l1", 5, 1),
        ("oak_t5_l2", 5, 2),
        ("oak_t5_l3", 5, 3),
    ];
    NAMES
        .iter()
        .find(|(candidate, _, _)| *candidate == name)
        .map(|(_, trunk, layout)| (*trunk, *layout))
}

/// Wall height for one of the three authored ruin names.
fn ruin_wall_geometry(name: &str) -> Option<i32> {
    const NAMES: [(&str, i32); 3] = [("ruin_wall_1", 1), ("ruin_wall_2", 2), ("ruin_wall_3", 3)];
    NAMES
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, wall_height)| *wall_height)
}

/// Oak invariants: fixed box, origin column trunk of the named height, the
/// layout's branch count, and an all-log/leaf body.
fn validate_oak_prefab(prefab: &Prefab, trunk: i32, layout: u8) -> Result<(), String> {
    if prefab.size != OAK_SIZE {
        return Err("oak prefab size must be [5, 6, 5]".to_owned());
    }
    if prefab.origin != STRUCTURE_ORIGIN {
        return Err("oak prefab origin must be [2, 0, 2]".to_owned());
    }
    if prefab
        .blocks
        .iter()
        .any(|block| !matches!(block.block, BLOCK_AIR | BLOCK_OAK_LOG | BLOCK_OAK_LEAVES))
    {
        return Err("oak prefab may only contain oak_log and oak_leaves".to_owned());
    }

    let trunk_cells = i32::try_from(
        prefab
            .blocks
            .iter()
            .filter(|block| {
                block.block == BLOCK_OAK_LOG
                    && block.position[0] == STRUCTURE_ORIGIN[0]
                    && block.position[2] == STRUCTURE_ORIGIN[2]
            })
            .count(),
    )
    .unwrap_or(i32::MAX);
    let contiguous = (0..trunk).all(|level| {
        prefab.blocks.iter().any(|block| {
            block.position == [STRUCTURE_ORIGIN[0], level as u32, STRUCTURE_ORIGIN[2]]
                && block.block == BLOCK_OAK_LOG
        })
    });
    let above_trunk = prefab.blocks.iter().any(|block| {
        block.position == [STRUCTURE_ORIGIN[0], trunk as u32, STRUCTURE_ORIGIN[2]]
            && block.block == BLOCK_OAK_LOG
    });
    if !contiguous || above_trunk || trunk_cells != trunk {
        return Err(format!(
            "oak prefab trunk must be {trunk} contiguous oak_log cells at the origin column"
        ));
    }

    let branch_cells = prefab
        .blocks
        .iter()
        .filter(|block| {
            block.block == BLOCK_OAK_LOG
                && (block.position[0] != STRUCTURE_ORIGIN[0]
                    || block.position[2] != STRUCTURE_ORIGIN[2])
        })
        .count();
    let expected = OAK_BRANCH_COUNTS[usize::from(layout)];
    if branch_cells != expected {
        return Err(format!(
            "oak prefab layout {layout} must have {expected} branch cells"
        ));
    }

    if !prefab
        .blocks
        .iter()
        .any(|block| block.block == BLOCK_OAK_LEAVES)
    {
        return Err("oak prefab must contain oak_leaves".to_owned());
    }
    Ok(())
}

/// Ruin invariants: fixed origin, `wall_height + 1` layers, cobble/brick only
/// and exactly the one typed loot marker the placement pass consumes.
fn validate_ruin_prefab(prefab: &Prefab, wall_height: i32) -> Result<(), String> {
    if prefab.origin != STRUCTURE_ORIGIN {
        return Err("ruin prefab origin must be [2, 0, 2]".to_owned());
    }
    let expected_height = wall_height + 1;
    if prefab.size != [5, expected_height as u32, 5] {
        return Err(format!(
            "ruin prefab size must be [5, {expected_height}, 5]"
        ));
    }
    if prefab
        .blocks
        .iter()
        .any(|block| !matches!(block.block, BLOCK_AIR | BLOCK_COBBLESTONE | BLOCK_BRICK))
    {
        return Err("ruin prefab may only contain cobblestone and brick".to_owned());
    }
    let marker = prefab.markers.as_slice();
    let has_marker = matches!(
        marker,
        [prefab_marker]
            if prefab_marker.position == [0, 0, 0]
                && prefab_marker.kind == MarkerKind::LootChest
                && prefab_marker.table == MarkerLootTable::Ruin
    );
    if !has_marker {
        return Err(
            "ruin prefab must carry exactly one loot_chest (ruin) marker at [0, 0, 0]".to_owned(),
        );
    }
    Ok(())
}

/// Run the sorted walk and render the **combined** `--validate-worldgen` report.
///
/// Line order (D350): the prefab section in walk order, then one
/// `error: worldgen/structures/<file>: <reason>` per rejected structure file,
/// then one `ok: structure <name>` per effective catalog name in ascending
/// order, then one `error: worldgen/structures: missing required structure
/// prefab '<name>'` per required name the catalog lacks, then the single
/// summary line `validate-worldgen: <p> prefabs, <s> structures, <e> errors, 0
/// warnings`.
///
/// Returns `(lines, ok)` with `ok == (errors == 0)`; the error count includes
/// prefab rejections, structure rejections and missing required names. A
/// missing tree is valid empty content for prefabs but always fails on the
/// eleven required structure names, because the builtin master is the
/// generation contract.
pub fn validate_worldgen_lines(root: &Path) -> (Vec<String>, bool) {
    let (mut lines, prefabs, prefab_errors) = prefab_section(root);
    let (catalog, diagnostics) = StructureCatalog::load_sorted(root);
    let mut errors = prefab_errors + diagnostics.len();
    for diagnostic in diagnostics {
        lines.push(format!("error: {diagnostic}"));
    }
    for name in catalog.names() {
        lines.push(format!("ok: structure {name}"));
    }
    for name in catalog.missing_required() {
        errors += 1;
        lines.push(format!(
            "error: worldgen/{STRUCTURES_SUBDIR}: missing required structure prefab '{name}'"
        ));
    }
    lines.push(format!(
        "validate-worldgen: {prefabs} prefabs, {} structures, {errors} errors, 0 warnings",
        catalog.len()
    ));
    (lines, errors == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    type CellMap = BTreeMap<[u32; 3], u16>;

    fn cell_map(prefab: &Prefab) -> CellMap {
        prefab
            .blocks
            .iter()
            .filter(|block| block.block != BLOCK_AIR)
            .map(|block| (block.position, block.block))
            .collect()
    }

    /// Independent oracle for the pre-slice ruin shell: `bottom`/`top` and the
    /// corner/wall material rule, re-derived here so the authored content is
    /// compared against the legacy formula and not against itself.
    fn legacy_ruin_cells(wall_height: i32) -> CellMap {
        let mut cells = CellMap::new();
        for dz in -2i32..=2 {
            for dx in -2i32..=2 {
                if dx.abs() != 2 && dz.abs() != 2 {
                    continue;
                }
                let corner = dx.abs() == 2 && dz.abs() == 2;
                let block = if corner {
                    BLOCK_BRICK
                } else {
                    BLOCK_COBBLESTONE
                };
                let bottom = if dx == 0 && dz == 2 { 2 } else { 1 };
                let top = wall_height + i32::from(corner);
                if bottom > top {
                    continue;
                }
                for level in bottom..=top {
                    cells.insert(
                        [(2 + dx) as u32, (level - 1) as u32, (2 + dz) as u32],
                        block,
                    );
                }
            }
        }
        cells
    }

    fn oak_name_parts(name: &str) -> (i32, u8) {
        let mut parts = name.split('_');
        assert_eq!(parts.next(), Some("oak"));
        let trunk = parts
            .next()
            .and_then(|part| part.strip_prefix('t'))
            .and_then(|digits| digits.parse::<i32>().ok())
            .expect("oak name has a tN segment");
        let layout = parts
            .next()
            .and_then(|part| part.strip_prefix('l'))
            .and_then(|digit| digit.parse::<u8>().ok())
            .expect("oak name has an lN segment");
        (trunk, layout)
    }

    fn palette_character(block: u16) -> char {
        match block {
            BLOCK_OAK_LOG => '#',
            BLOCK_OAK_LEAVES => '%',
            BLOCK_COBBLESTONE => '+',
            BLOCK_BRICK => '*',
            BLOCK_LOOT_CHEST => '!',
            other => panic!("test renderer has no palette character for block {other}"),
        }
    }

    /// Render a prefab document from an explicit cell map, used to write
    /// runtime overlay fixtures without duplicating the authored text.
    fn render_prefab(
        prefab: &Prefab,
        cells: &CellMap,
        markers: &[crate::prefab::PrefabMarker],
    ) -> String {
        let mut out = String::new();
        out.push_str("format = 1\n");
        out.push_str(&format!("name = \"{}\"\n", prefab.name));
        out.push_str(&format!(
            "origin = [{}, {}, {}]\n",
            prefab.origin[0], prefab.origin[1], prefab.origin[2]
        ));
        out.push_str(&format!(
            "size = [{}, {}, {}]\n\n",
            prefab.size[0], prefab.size[1], prefab.size[2]
        ));
        out.push_str("layers = [\n");
        for y in 0..prefab.size[1] {
            let mut rows = Vec::with_capacity(prefab.size[2] as usize);
            for z in 0..prefab.size[2] {
                let mut row = String::with_capacity(prefab.size[0] as usize);
                for x in 0..prefab.size[0] {
                    match cells.get(&[x, y, z]) {
                        Some(block) => row.push(palette_character(*block)),
                        None => row.push('.'),
                    }
                }
                rows.push(format!("\"{row}\""));
            }
            out.push_str("  [ ");
            out.push_str(&rows.join(", "));
            out.push_str(" ],\n");
        }
        out.push_str("]\n\n[palette]\n");
        out.push_str("\"#\" = \"oak_log\"\n");
        out.push_str("\"%\" = \"oak_leaves\"\n");
        out.push_str("\"+\" = \"cobblestone\"\n");
        out.push_str("\"*\" = \"brick\"\n");
        out.push_str("\"!\" = \"loot_chest\"\n");
        for marker in markers {
            out.push_str("\n[[markers]]\n");
            out.push_str(&format!("kind = \"{}\"\n", marker.kind.as_str()));
            out.push_str(&format!("table = \"{}\"\n", marker.table.as_str()));
            out.push_str(&format!(
                "position = [{}, {}, {}]\n",
                marker.position[0], marker.position[1], marker.position[2]
            ));
        }
        out
    }

    fn write_structure(root: &Path, file_name: &str, text: &str) {
        let dir = root.join(STRUCTURES_SUBDIR);
        fs::create_dir_all(&dir).expect("create structures fixture dir");
        fs::write(dir.join(file_name), text).expect("write structure fixture");
    }

    fn absent_root() -> PathBuf {
        let temp = tempfile::tempdir().expect("tempdir");
        temp.path().join("absent")
    }

    #[test]
    fn builtin_is_complete_and_valid() {
        let catalog = StructureCatalog::builtin();
        assert_eq!(catalog.len(), 11);
        assert!(!catalog.is_empty());
        assert_eq!(
            catalog.names().collect::<Vec<_>>(),
            OAK_PREFABS
                .iter()
                .chain(RUIN_PREFABS.iter())
                .copied()
                .collect::<Vec<_>>()
        );
        assert!(catalog.missing_required().is_empty());
        assert_eq!(
            StructureCatalog::builtin().fingerprint(),
            catalog.fingerprint(),
            "the builtin master must be parsed once and stay stable"
        );
        assert!(
            std::ptr::eq(StructureCatalog::builtin(), catalog),
            "builtin() must hand out the same OnceLock value"
        );

        for trunk in [4, 5] {
            for layout in 0..4u8 {
                let prefab = catalog
                    .oak(trunk, layout)
                    .unwrap_or_else(|| panic!("oak_t{trunk}_l{layout} must be builtin"));
                assert_eq!(prefab.name, format!("oak_t{trunk}_l{layout}"));
                assert_eq!(prefab.size, OAK_SIZE);
                assert_eq!(prefab.origin, STRUCTURE_ORIGIN);
                assert!(prefab.markers.is_empty());
            }
        }
        for wall_height in 1..=3 {
            let prefab = catalog
                .ruin(wall_height)
                .unwrap_or_else(|| panic!("ruin_wall_{wall_height} must be builtin"));
            assert_eq!(prefab.name, format!("ruin_wall_{wall_height}"));
            assert_eq!(prefab.size, [5, (wall_height + 1) as u32, 5]);
            assert_eq!(prefab.origin, STRUCTURE_ORIGIN);
            assert_eq!(prefab.markers.len(), 1);
        }

        assert!(catalog.oak(6, 0).is_none());
        assert!(catalog.oak(4, 4).is_none());
        assert!(catalog.ruin(4).is_none());

        assert_eq!(catalog.fingerprint(), 0xb5e7_41fd_23a8_2fd4);
    }

    #[test]
    fn every_builtin_master_matches_its_checked_in_file() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../worldgen");
        for name in OAK_PREFABS.iter().chain(RUIN_PREFABS.iter()) {
            let path = root.join(STRUCTURES_SUBDIR).join(format!("{name}.toml"));
            assert!(
                path.is_file(),
                "{} is the tracked authoring surface",
                path.display()
            );
        }
        let (catalog, diagnostics) = StructureCatalog::load_sorted(&root);
        assert!(diagnostics.is_empty(), "diagnostics: {diagnostics:?}");
        assert_eq!(catalog, *StructureCatalog::builtin());

        let (lines, ok) = validate_worldgen_lines(&root);
        assert!(ok, "the checked-in tree must validate clean: {lines:?}");
        assert_eq!(
            lines.last().map(String::as_str),
            Some("validate-worldgen: 3 prefabs, 11 structures, 0 errors, 0 warnings")
        );
    }

    #[test]
    fn ruin_prefabs_reproduce_the_legacy_shell() {
        let catalog = StructureCatalog::builtin();
        for wall_height in 1..=3 {
            let prefab = catalog.ruin(wall_height).expect("ruin prefab is builtin");
            assert_eq!(
                cell_map(prefab),
                legacy_ruin_cells(wall_height),
                "ruin_wall_{wall_height} drifted from the legacy shell"
            );
            assert_eq!(
                prefab.blocks.len(),
                5 * prefab.size[1] as usize * 5,
                "every grid cell must be materialized"
            );
            let marker = prefab
                .markers
                .first()
                .expect("ruin carries its loot marker");
            assert_eq!(marker.position, [0, 0, 0]);
            assert_eq!(marker.kind, MarkerKind::LootChest);
            assert_eq!(marker.table, MarkerLootTable::Ruin);
        }
    }

    #[test]
    fn oak_prefabs_obey_the_model_bounds() {
        let catalog = StructureCatalog::builtin();
        for name in OAK_PREFABS {
            let prefab = catalog.get(name).expect("oak prefab is builtin");
            let (trunk, layout) = oak_name_parts(name);

            let trunk_cells = prefab
                .blocks
                .iter()
                .filter(|block| {
                    block.block == BLOCK_OAK_LOG
                        && block.position[0] == STRUCTURE_ORIGIN[0]
                        && block.position[2] == STRUCTURE_ORIGIN[2]
                })
                .count();
            assert_eq!(trunk_cells, trunk as usize, "{name} trunk height");

            let branch_cells = prefab
                .blocks
                .iter()
                .filter(|block| {
                    block.block == BLOCK_OAK_LOG
                        && (block.position[0] != STRUCTURE_ORIGIN[0]
                            || block.position[2] != STRUCTURE_ORIGIN[2])
                })
                .count();
            assert_eq!(
                branch_cells,
                OAK_BRANCH_COUNTS[usize::from(layout)],
                "{name} branch count"
            );

            let mut saw_leaf = false;
            for block in &prefab.blocks {
                match block.block {
                    BLOCK_AIR => continue,
                    BLOCK_OAK_LOG => {}
                    BLOCK_OAK_LEAVES => saw_leaf = true,
                    other => panic!("{name} contains unexpected block {other}"),
                }
                assert!(
                    block.position[0] <= 4 && block.position[2] <= 4,
                    "{name} cell {:?} leaves the 5x5 footprint",
                    block.position
                );
                assert!(
                    block.position[1] <= 5,
                    "{name} cell {:?} exceeds the 6-layer box",
                    block.position
                );
            }
            assert!(saw_leaf, "{name} must carry a canopy");
        }
    }

    #[test]
    fn load_sorted_overlays_and_falls_back() {
        let builtin = StructureCatalog::builtin();
        let builtin_fingerprint = builtin.fingerprint();

        // (a) A valid runtime edit overlays by name and moves the fingerprint.
        let temp = tempfile::tempdir().expect("tempdir");
        let oak = builtin.get("oak_t4_l0").expect("builtin oak");
        let mut edited_cells = cell_map(oak);
        edited_cells.insert([0, 0, 0], BLOCK_OAK_LEAVES);
        write_structure(
            temp.path(),
            "oak_t4_l0.toml",
            &render_prefab(oak, &edited_cells, &[]),
        );
        let (catalog, diagnostics) = StructureCatalog::load_sorted(temp.path());
        assert!(diagnostics.is_empty(), "diagnostics: {diagnostics:?}");
        assert_eq!(catalog.len(), builtin.len());
        assert!(catalog.missing_required().is_empty());
        assert_ne!(catalog.fingerprint(), builtin_fingerprint);
        assert_ne!(
            catalog.get("oak_t4_l0"),
            builtin.get("oak_t4_l0"),
            "the accepted file must replace the builtin entry"
        );
        assert_eq!(
            catalog.get("oak_t4_l1"),
            builtin.get("oak_t4_l1"),
            "untouched names keep the builtin"
        );

        // (b) Broken TOML: exactly one diagnostic, the builtin keeps generating.
        fs::write(
            temp.path().join(STRUCTURES_SUBDIR).join("ruin_wall_1.toml"),
            "format = [\n",
        )
        .expect("write broken fixture");
        let (catalog, diagnostics) = StructureCatalog::load_sorted(temp.path());
        assert_eq!(
            diagnostics,
            vec!["worldgen/structures/ruin_wall_1.toml: invalid TOML".to_owned()]
        );
        assert_eq!(catalog.len(), builtin.len());
        assert_eq!(
            catalog.get("ruin_wall_1"),
            builtin.get("ruin_wall_1"),
            "a rejected file must leave the builtin of that name in place"
        );

        // (c) A wrong file stem is named.
        let temp = tempfile::tempdir().expect("tempdir");
        let ruin = builtin.get("ruin_wall_1").expect("builtin ruin");
        write_structure(
            temp.path(),
            "wrong_stem.toml",
            &render_prefab(ruin, &cell_map(ruin), &ruin.markers),
        );
        let (catalog, diagnostics) = StructureCatalog::load_sorted(temp.path());
        assert_eq!(
            diagnostics,
            vec![
                "worldgen/structures/wrong_stem.toml: file name does not match prefab name 'ruin_wall_1'"
                    .to_owned()
            ]
        );
        assert_eq!(catalog.get("ruin_wall_1"), builtin.get("ruin_wall_1"));

        // (d) A family-invalid file is named and falls back.
        let temp = tempfile::tempdir().expect("tempdir");
        write_structure(
            temp.path(),
            "ruin_wall_1.toml",
            &render_prefab(ruin, &cell_map(ruin), &[]),
        );
        let (catalog, diagnostics) = StructureCatalog::load_sorted(temp.path());
        assert_eq!(
            diagnostics,
            vec![
                "worldgen/structures/ruin_wall_1.toml: ruin prefab must carry exactly one loot_chest (ruin) marker at [0, 0, 0]"
                    .to_owned()
            ]
        );
        assert_eq!(catalog.get("ruin_wall_1"), builtin.get("ruin_wall_1"));
    }

    #[test]
    fn load_sorted_rejects_a_generic_loot_chest_block() {
        let builtin = StructureCatalog::builtin();
        let oak = builtin.get("oak_t4_l0").expect("builtin oak");
        let mut cells = cell_map(oak);
        cells.insert([0, 0, 0], BLOCK_LOOT_CHEST);
        let temp = tempfile::tempdir().expect("tempdir");
        write_structure(
            temp.path(),
            "oak_t4_l0.toml",
            &render_prefab(oak, &cells, &[]),
        );

        let (catalog, diagnostics) = StructureCatalog::load_sorted(temp.path());
        assert_eq!(
            diagnostics,
            vec![
                "worldgen/structures/oak_t4_l0.toml: structure prefab may not contain a loot_chest block; use a [[markers]] entry"
                    .to_owned()
            ]
        );
        assert_eq!(catalog.get("oak_t4_l0"), builtin.get("oak_t4_l0"));
    }

    #[test]
    fn validate_report_lists_structures_and_fails_on_a_broken_file() {
        let root = absent_root();
        let (lines, ok) = validate_worldgen_lines(&root);
        let mut expected: Vec<String> = OAK_PREFABS
            .iter()
            .chain(RUIN_PREFABS.iter())
            .map(|name| format!("ok: structure {name}"))
            .collect();
        expected
            .push("validate-worldgen: 0 prefabs, 11 structures, 0 errors, 0 warnings".to_owned());
        assert_eq!(lines, expected);
        assert!(ok);

        let temp = tempfile::tempdir().expect("tempdir");
        write_structure(temp.path(), "ruin_wall_2.toml", "format = [\n");
        let (lines, ok) = validate_worldgen_lines(temp.path());
        assert!(!ok);
        assert!(
            lines.contains(&"error: worldgen/structures/ruin_wall_2.toml: invalid TOML".to_owned()),
            "lines: {lines:?}"
        );
        assert_eq!(
            lines.last().map(String::as_str),
            Some("validate-worldgen: 0 prefabs, 11 structures, 1 errors, 0 warnings")
        );
        assert_eq!(lines.len(), 11 + 1 + 1);
    }
}
