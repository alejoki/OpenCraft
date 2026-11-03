//! Immutable content registry and built-in block definitions.
//!
//! [`ContentRegistry::builtin`] is the single authoritative built-in content
//! table. It is immutable for the session, is `Send + Sync`, and later slices
//! freeze it behind `Arc<ContentRegistry>` once pack-defined content exists.
//!
//! This module deliberately has no render, audio, settings, or filesystem
//! dependency: content identity must stay decoupled from presentation and
//! persistence so the pack parser (S02) can build a registry without touching
//! the GPU or the save layer.

pub mod names;
pub mod spec;

pub use spec::{ContentDiagnostic, PackContentSpec, Severity};

use std::sync::OnceLock;

use crate::world::block::{BlockId, BUILTIN_BLOCK_COUNT};

/// A resolved block definition.
///
/// Built-in definitions are derived from the existing [`BlockId`] accessors so
/// "registry equals built-ins" holds by construction. [`BlockDef::drop`] is the
/// S02/S05 staging type: it widens to `Option<ItemId>` once non-block drops and
/// per-field pack patches exist.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockDef {
    /// Stable identity this definition describes.
    pub id: BlockId,
    /// Unqualified stable name used by packs and saves (for example `oak_log`).
    pub name: String,
    /// Human-readable display name (for example `Oak Log`).
    pub display_name: String,
    /// Texture-array layer per face direction: +X, -X, +Y, -Y, +Z, -Z.
    pub faces: [u8; 6],
    /// Seconds of held left-click needed to break, or `None` if unbreakable.
    pub hardness: Option<f32>,
    /// Block item produced on break, or `None` for uncarryable blocks.
    ///
    /// This is the S02/S05 staging type that widens to `Option<ItemId>`.
    pub drop: Option<BlockId>,
    /// Whether this definition is placeholder art awaiting pack content.
    pub placeholder: bool,
}

impl BlockDef {
    /// Derive the built-in definition for `id`, or `None` for modded ids.
    pub fn from_builtin(id: BlockId) -> Option<BlockDef> {
        if !id.is_builtin() {
            return None;
        }
        let name = names::builtin_block_id_name(id)?.to_string();
        let display_name = id.name().to_string();
        let faces = [0u8, 1, 2, 3, 4, 5].map(|face| id.face_tex_layer(face));
        let hardness = id.break_duration_secs();
        let drop = if hardness.is_some() && id.is_carryable() {
            Some(id.mined_drop())
        } else {
            None
        };
        Some(BlockDef {
            id,
            name,
            display_name,
            faces,
            hardness,
            drop,
            placeholder: false,
        })
    }
}

/// Immutable, session-scoped registry of block definitions.
///
/// The built-in registry is created once and shared through a `OnceLock`.
/// It is `Send + Sync`; later slices freeze it behind `Arc<ContentRegistry>`
/// and layer pack-defined entries on top without mutating the built-in table.
#[derive(Debug)]
pub struct ContentRegistry {
    blocks: Vec<BlockDef>,
}

impl ContentRegistry {
    /// The immutable built-in registry, built once per process.
    pub fn builtin() -> &'static ContentRegistry {
        static REGISTRY: OnceLock<ContentRegistry> = OnceLock::new();
        REGISTRY.get_or_init(|| {
            let blocks = (0..BUILTIN_BLOCK_COUNT)
                .map(|raw| {
                    BlockDef::from_builtin(BlockId::from_raw(raw))
                        .expect("every built-in raw id has a definition")
                })
                .collect();
            ContentRegistry { blocks }
        })
    }

    /// All registered block definitions in raw id order.
    pub fn blocks(&self) -> &[BlockDef] {
        &self.blocks
    }

    /// The definition for `id`, or `None` when the id is unknown or modded.
    ///
    /// Indexing is by raw id and additionally filters on identity, so a forged
    /// [`BlockId::Modded`] payload can never alias a built-in definition.
    pub fn block(&self, id: BlockId) -> Option<&BlockDef> {
        if !id.is_builtin() {
            return None;
        }
        self.blocks
            .get(usize::from(id.to_raw()))
            .filter(|def| def.id == id)
    }

    /// The definition whose stable short name is exactly `name`.
    ///
    /// This is a linear scan over the frozen built-in table and matches the
    /// unqualified stable name, not the display name.
    pub fn block_by_name(&self, name: &str) -> Option<&BlockDef> {
        self.blocks.iter().find(|def| def.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_covers_every_builtin_block() {
        let registry = ContentRegistry::builtin();
        assert_eq!(
            registry.blocks().len(),
            usize::from(BUILTIN_BLOCK_COUNT),
            "the built-in registry must cover every built-in id"
        );

        for raw in 0..BUILTIN_BLOCK_COUNT {
            let id = BlockId::from_raw(raw);
            let def = registry
                .block(id)
                .unwrap_or_else(|| panic!("missing definition for raw {raw}"));

            assert_eq!(def.id, id);
            assert_eq!(def.name, names::BLOCK_ID_NAMES[usize::from(raw)]);
            assert_eq!(def.display_name, id.name());

            let expected_faces = [0u8, 1, 2, 3, 4, 5].map(|face| id.face_tex_layer(face));
            assert_eq!(def.faces, expected_faces, "faces mismatch for {id:?}");

            assert_eq!(def.hardness, id.break_duration_secs());
            let expected_drop = if id.break_duration_secs().is_some() && id.is_carryable() {
                Some(id.mined_drop())
            } else {
                None
            };
            assert_eq!(def.drop, expected_drop, "drop mismatch for {id:?}");
            assert!(!def.placeholder, "built-ins are not placeholders");

            assert_eq!(
                registry
                    .block_by_name(def.name.as_str())
                    .map(|found| found.id),
                Some(id),
                "name lookup mismatch for {id:?}"
            );
        }
    }

    #[test]
    fn registry_rejects_unknown_modded_ids() {
        let registry = ContentRegistry::builtin();
        assert!(registry
            .block(BlockId::Modded(BUILTIN_BLOCK_COUNT))
            .is_none());
        assert!(registry.block(BlockId::Modded(u16::MAX)).is_none());
        assert!(registry.block_by_name("mymod:ruby").is_none());
    }

    #[test]
    fn registry_is_send_sync_and_static() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ContentRegistry>();
        assert!(std::ptr::eq(
            ContentRegistry::builtin(),
            ContentRegistry::builtin()
        ));
    }
}
