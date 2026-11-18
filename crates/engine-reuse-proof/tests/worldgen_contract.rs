//! External contract for the device-free worldgen half of the reuse proof.
//!
//! Imports come from the consumer crate and `opencraft_worldgen`'s public
//! modules on purpose: this target proves the terrain generator and the
//! prefab/village codecs and planner are usable from an outside crate that
//! never imports `opencraft`, reads `worldgen/` at runtime or creates a GPU or
//! file handle.

use engine_reuse_proof::report::ProofReport;
use engine_reuse_proof::worldgen;
use opencraft_worldgen::prefab::PrefabError;

/// The whole measured terrain line, not a substring: any generation drift is a
/// contract break and must be re-pinned deliberately. Measured from a real
/// `cargo run -q -p engine-reuse-proof --locked`.
const TERRAIN_LINE: &str =
    "worldgen.terrain=chunks=4,uniform=1,non_air=53951,digest=0xa13b53f3403b98bf";

/// The whole measured village line, not a substring: any plan change is a
/// contract break and must be re-pinned deliberately.
const VILLAGE_LINE: &str = "worldgen.village_plan=cell=-6,-7,prefabs=2,links=1,roads=4";

#[test]
fn worldgen_proof_plans_a_linked_village() {
    let proof = worldgen::run();

    assert_eq!(proof.village_cell, (-6, -7));
    assert_eq!(proof.prefabs, 2, "one hub plus one child placement");
    assert_eq!(proof.links, 1, "the child connects to one hub socket");
    assert_eq!(proof.roads, 4, "the one link gets a four-cell road");

    assert_eq!(proof.terrain_chunks, 4, "four measured terrain chunks");
    assert_eq!(proof.terrain_uniform, 1, "only the sky chunk is uniform");
    assert_eq!(
        proof.terrain_non_air, 53_951,
        "pinned non-air cells across the four chunks"
    );
    assert_eq!(
        proof.terrain_digest, 0xa13b_53f3_403b_98bf,
        "pinned terrain digest in game baseline order"
    );

    let repeated = worldgen::run();
    assert_eq!(
        (proof.village_cell, proof.prefabs, proof.links, proof.roads,),
        (
            repeated.village_cell,
            repeated.prefabs,
            repeated.links,
            repeated.roads,
        ),
        "the village proof must be deterministic across runs"
    );
    assert_eq!(
        (
            proof.terrain_chunks,
            proof.terrain_uniform,
            proof.terrain_non_air,
            proof.terrain_digest,
        ),
        (
            repeated.terrain_chunks,
            repeated.terrain_uniform,
            repeated.terrain_non_air,
            repeated.terrain_digest,
        ),
        "the terrain proof must be deterministic across runs"
    );
}

#[test]
fn proof_report_renders_exactly_the_pinned_worldgen_line() {
    let rendered = ProofReport::to_string(&engine_reuse_proof::run());
    let lines: Vec<&str> = rendered.lines().collect();

    assert_eq!(lines.len(), 22, "six proof groups of lines");
    assert_eq!(
        lines.get(lines.len() - 2).copied(),
        Some(TERRAIN_LINE),
        "the terrain proof renders immediately before the village line:\n{rendered}"
    );
    assert_eq!(
        lines.last().copied(),
        Some(VILLAGE_LINE),
        "the worldgen proof is still the final report line:\n{rendered}"
    );
    assert!(
        rendered.contains('\n'),
        "the report must stay newline-terminated"
    );
}

#[test]
fn two_report_renders_are_byte_identical() {
    let first = engine_reuse_proof::run().to_string();
    let second = engine_reuse_proof::run().to_string();

    assert_eq!(
        first, second,
        "the whole 22-line report must be byte-stable"
    );
    assert_eq!(first.lines().count(), 22);
}

#[test]
fn worldgen_proof_source_stays_in_memory_only() {
    let source = include_str!("../src/worldgen.rs");
    for forbidden in [
        "std::fs",
        "File::",
        "std::env",
        "SystemTime",
        "Instant",
        "thread::",
        "wgpu",
        "tempfile",
        "opencraft::",
    ] {
        assert!(
            !source.contains(forbidden),
            "the worldgen proof must stay in-memory and game-free; found {forbidden:?}"
        );
    }
}

#[test]
fn malformed_embedded_prefab_fails_closed() {
    // A missing [palette] must surface the crate's typed error, never a panic
    // or a zeroed prefab: the proof's decoder path is the production codec.
    assert!(matches!(
        worldgen::parse_malformed_prefab(),
        Err(PrefabError::MissingPalette)
    ));
}
