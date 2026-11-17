//! Aggregated, deterministic [`ProofReport`] over the six proof areas.
//!
//! The report is the runnable surface of the consumer: the `engine-reuse-proof`
//! bin prints it and exits 0. Every line is a `key=value` pair, floats use a
//! fixed four-decimal precision, and nothing here reads a clock, host path,
//! environment variable or secret. Output is therefore byte-identical on every
//! run and every platform.

use std::fmt;

use crate::content::ContentProof;
use crate::gfx::GfxProof;
use crate::motion::MotionProof;
use crate::physics::PhysicsProof;
use crate::storage::StorageProof;
use crate::worldgen::WorldgenProof;

/// The aggregated result of one device-free consumer proof run.
#[derive(Clone, Copy, Debug)]
pub struct ProofReport {
    /// Physics stepping, raycast and sweep proof.
    pub physics: PhysicsProof,
    /// Mesh, texture-array and model ABI proof.
    pub gfx: GfxProof,
    /// Steering and deterministic A* proof.
    pub motion: MotionProof,
    /// Archive container and chunk-section codec proof.
    pub storage: StorageProof,
    /// Pack-parse and asset-pipeline proof.
    pub content: ContentProof,
    /// In-memory prefab/village planning proof.
    pub worldgen: WorldgenProof,
}

/// Format a proof float with fixed precision, normalizing negative zero.
///
/// The solver's `ceil` landing clamp can produce `-0.0`, which would render as
/// `-0.0000`. Normalizing keeps the report byte-stable and readable.
struct Fixed(f32);

impl fmt::Display for Fixed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = if self.0 == 0.0 { 0.0 } else { self.0 };
        write!(f, "{value:.4}")
    }
}

impl fmt::Display for ProofReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "physics.landing_y={}",
            Fixed(self.physics.landing.position.y)
        )?;
        writeln!(f, "physics.wall_x={}", Fixed(self.physics.wall.position.x))?;
        writeln!(
            f,
            "physics.ray_block={},{},{}",
            self.physics.ray_hit.block_pos.x,
            self.physics.ray_hit.block_pos.y,
            self.physics.ray_hit.block_pos.z
        )?;
        writeln!(f, "physics.sweep_t={}", Fixed(self.physics.sweep_t))?;

        writeln!(f, "gfx.opaque_bytes={}", self.gfx.opaque_bytes)?;
        writeln!(f, "gfx.water_bytes={}", self.gfx.water_bytes)?;
        writeln!(
            f,
            "gfx.texture_layer_bytes={}",
            self.gfx.texture_layer_bytes
        )?;
        writeln!(
            f,
            "gfx.mip_bytes={},{},{},{},{}",
            self.gfx.mip_bytes[0],
            self.gfx.mip_bytes[1],
            self.gfx.mip_bytes[2],
            self.gfx.mip_bytes[3],
            self.gfx.mip_bytes[4]
        )?;
        writeln!(f, "gfx.model_vertex_bytes={}", self.gfx.model_vertex_bytes)?;

        writeln!(f, "motion.steered_z={}", Fixed(self.motion.steered_feet.z))?;
        writeln!(f, "motion.step_up_y={}", Fixed(self.motion.step_up_feet.y))?;
        writeln!(f, "motion.stuck={}", Fixed(self.motion.stuck))?;
        writeln!(
            f,
            "motion.airborne_z={}",
            Fixed(self.motion.airborne_feet.z)
        )?;
        writeln!(f, "motion.path_len={}", self.motion.path_len)?;
        writeln!(f, "motion.deterministic={}", self.motion.deterministic)?;

        writeln!(f, "storage.archive_bytes={}", self.storage.archive_bytes)?;
        writeln!(f, "storage.section_count={}", self.storage.section_count)?;
        writeln!(f, "storage.chunk_edits={}", self.storage.chunk_edits)?;

        writeln!(f, "content.pack_entries={}", self.content.pack_entries)?;
        writeln!(
            f,
            "content.catalog_digest=0x{:016x}",
            self.content.catalog_digest
        )?;

        writeln!(
            f,
            "worldgen.village_plan=cell={},{},prefabs={},links={},roads={}",
            self.worldgen.village_cell.0,
            self.worldgen.village_cell.1,
            self.worldgen.prefabs,
            self.worldgen.links,
            self.worldgen.roads
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYS: [&str; 21] = [
        "physics.landing_y=",
        "physics.wall_x=",
        "physics.ray_block=",
        "physics.sweep_t=",
        "gfx.opaque_bytes=",
        "gfx.water_bytes=",
        "gfx.texture_layer_bytes=",
        "gfx.mip_bytes=",
        "gfx.model_vertex_bytes=",
        "motion.steered_z=",
        "motion.step_up_y=",
        "motion.stuck=",
        "motion.airborne_z=",
        "motion.path_len=",
        "motion.deterministic=",
        "storage.archive_bytes=",
        "storage.section_count=",
        "storage.chunk_edits=",
        "content.pack_entries=",
        "content.catalog_digest=",
        "worldgen.village_plan=",
    ];

    #[test]
    fn display_renders_key_value_groups_without_non_finite_values() {
        let report = ProofReport {
            physics: crate::physics::run(),
            gfx: crate::gfx::run(),
            motion: crate::motion::run(),
            storage: crate::storage::run(),
            content: crate::content::run(),
            worldgen: crate::worldgen::run(),
        };
        let rendered = report.to_string();

        assert_eq!(
            rendered,
            report.to_string(),
            "Display must render identically twice"
        );

        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines.len(), KEYS.len(), "one line per proof key");
        for (line, key) in lines.iter().zip(KEYS) {
            assert!(
                line.starts_with(key),
                "line {line:?} must start with {key:?}"
            );
            assert!(
                line.split_once('=')
                    .is_some_and(|(_, value)| !value.is_empty()),
                "line {line:?} must carry a non-empty value"
            );
        }

        for token in ["NaN", "nan", "inf", "Inf", "infinity", "Infinity"] {
            assert!(
                !rendered.contains(token),
                "rendered report must contain no {token:?}:\n{rendered}"
            );
        }

        // The report is data only: no host paths, timing or multiline dumps.
        for token in ["\\", "/", "\t", "  "] {
            assert!(
                !rendered.contains(token),
                "rendered report must contain no {token:?}:\n{rendered}"
            );
        }
    }
}
