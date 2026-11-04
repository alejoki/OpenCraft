//! Precomputed per-block face texture-layer table.
//!
//! The greedy mesher runs inside rayon workers that must not hold an
//! `Arc<ContentRegistry>`: threading the registry through the mesh job would
//! couple render-rate work to content lookup and keep a session-scoped table
//! alive per worker. Instead the session flattens the registry once into a
//! dense table (`raw id * 6 + face`) and hands the workers a cheap shared
//! slice. Each visible face then costs one slice index in the inner loop.
//!
//! The built-in table is derived from [`BlockId::face_tex_layer`] so the
//! pre-M010 mesher output is byte-identical by construction.

use std::sync::{Arc, OnceLock};

use crate::content::{ContentRegistry, PLACEHOLDER_BLOCK_LAYER};
use crate::world::block::{BlockId, BUILTIN_BLOCK_COUNT};

/// One entry per face direction: +X, -X, +Y, -Y, +Z, -Z.
const FACES_PER_BLOCK: usize = 6;

/// Flat, immutable texture-array layer table for every registered block.
///
/// Entries are stored in raw-id order, so the lookup index is
/// `block.to_raw() * 6 + face`. Unknown ids (raw past the table) and
/// out-of-range face directions fall back to the reserved checker layer
/// [`PLACEHOLDER_BLOCK_LAYER`] rather than indexing out of bounds or panicking
/// in a mesh worker.
#[derive(Debug)]
pub struct BlockFaces {
    layers: Box<[u8]>,
}

impl BlockFaces {
    /// Flattens `registry`'s block definitions into a face-layer table.
    ///
    /// Every definition is included in raw-id order, placeholders included, so
    /// the table length is exactly `registry.blocks().len() * 6` and a modded
    /// id allocated by the registry resolves to its authored (or placeholder)
    /// faces.
    pub fn from_registry(registry: &ContentRegistry) -> BlockFaces {
        let mut layers = vec![PLACEHOLDER_BLOCK_LAYER; registry.blocks().len() * FACES_PER_BLOCK];
        for def in registry.blocks() {
            // Registry definitions are dense in raw-id order (built-ins first,
            // pack blocks and placeholders appended), so every start offset
            // lands inside the table. Placing by id rather than by iteration
            // index keeps the mapping correct even if that invariant ever
            // loosens, and a definition that does not fit simply cannot be
            // addressed and falls back to the placeholder layer.
            let start = usize::from(def.id.to_raw()) * FACES_PER_BLOCK;
            if let Some(slot) = layers.get_mut(start..start + FACES_PER_BLOCK) {
                slot.copy_from_slice(&def.faces);
            }
        }
        BlockFaces {
            layers: layers.into_boxed_slice(),
        }
    }

    /// The texture-array layer for `block`'s `face`.
    ///
    /// `face` is the packed quad direction: 0 = +X, 1 = -X, 2 = +Y, 3 = -Y,
    /// 4 = +Z, 5 = -Z. Any forged raw id below the built-in count, any raw id
    /// past the table, and any face above 5 return
    /// [`PLACEHOLDER_BLOCK_LAYER`].
    #[inline]
    pub fn layer(&self, block: BlockId, face: u8) -> u8 {
        if usize::from(face) >= FACES_PER_BLOCK {
            return PLACEHOLDER_BLOCK_LAYER;
        }
        // A forged `Modded` payload below the built-in count is not a registry
        // index; `to_raw` also debug-asserts on it, so reject it first.
        let raw = match block {
            BlockId::Modded(raw) if raw < BUILTIN_BLOCK_COUNT => {
                return PLACEHOLDER_BLOCK_LAYER;
            }
            other => other.to_raw(),
        };
        self.layers
            .get(usize::from(raw) * FACES_PER_BLOCK + usize::from(face))
            .copied()
            .unwrap_or(PLACEHOLDER_BLOCK_LAYER)
    }

    /// The number of face entries in the table (`blocks * 6`).
    pub fn len(&self) -> usize {
        self.layers.len()
    }

    /// Whether the table has no entries (a registry with no blocks).
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    /// The built-in face table, built once per process and shared behind an
    /// `Arc`. Every pre-M010 mesher caller resolves through this.
    pub fn builtin() -> Arc<BlockFaces> {
        static FACES: OnceLock<Arc<BlockFaces>> = OnceLock::new();
        Arc::clone(
            FACES.get_or_init(|| Arc::new(BlockFaces::from_registry(ContentRegistry::builtin()))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::spec::{BlockEntry, BlockTextures, PackAsset};
    use crate::content::{build_registry, PackContentSpec, PackIdentity, PACK_LAYER_BASE};
    use std::collections::BTreeMap;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn block_faces_is_send_sync_and_shared() {
        assert_send_sync::<BlockFaces>();
        let first = BlockFaces::builtin();
        let second = BlockFaces::builtin();
        assert!(Arc::ptr_eq(&first, &second), "builtin table is a OnceLock");
        assert_eq!(first.len(), usize::from(BUILTIN_BLOCK_COUNT) * 6);
        assert!(!first.is_empty());
        assert_eq!(
            BlockFaces::from_registry(ContentRegistry::builtin()).len(),
            first.len()
        );
    }

    #[test]
    fn builtin_layers_match_block_id_accessors_exhaustively() {
        let faces = BlockFaces::builtin();
        for raw in 0..BUILTIN_BLOCK_COUNT {
            let block = BlockId::from_raw(raw);
            for face in 0u8..6 {
                assert_eq!(
                    faces.layer(block, face),
                    block.face_tex_layer(face),
                    "mismatch for {block:?} face {face}"
                );
            }
        }
    }

    #[test]
    fn pack_definition_maps_every_face_to_its_authored_layer() {
        let mut spec = PackContentSpec::default();
        spec.assets.insert(
            "ruby.png".to_string(),
            PackAsset {
                rgba: vec![0x5A; 1024],
                source_side: 16,
            },
        );
        spec.blocks.push(BlockEntry {
            id: "mymod:ruby_block".to_string(),
            display_name: None,
            textures: BlockTextures {
                all: Some("ruby.png".to_string()),
                ..BlockTextures::default()
            },
            hardness: 1.5,
            drop: None,
            tool: None,
            stack_limit: 64,
        });

        let (registry, report) = build_registry(
            ContentRegistry::builtin(),
            &[(
                PackIdentity {
                    folder: "mymod".to_string(),
                    version: Some("1.0".to_string()),
                },
                spec,
            )],
        );
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);

        let id = BlockId::Modded(BUILTIN_BLOCK_COUNT);
        let faces = BlockFaces::from_registry(&registry);
        assert_eq!(faces.len(), registry.blocks().len() * 6);
        for face in 0u8..6 {
            assert_eq!(
                faces.layer(id, face),
                PACK_LAYER_BASE,
                "pack block face {face} must use its authored layer"
            );
        }
        // Built-in entries in the same table still resolve to built-in art.
        assert_eq!(
            faces.layer(BlockId::OakLog, 2),
            BlockId::OakLog.face_tex_layer(2)
        );
    }

    #[test]
    fn appended_placeholder_block_maps_to_the_reserved_checker() {
        let mut registry = ContentRegistry::builtin().clone();
        let id = registry.append_placeholder_block("mymod:missing");
        let faces = BlockFaces::from_registry(&registry);
        assert_eq!(faces.len(), registry.blocks().len() * 6);
        for face in 0u8..6 {
            assert_eq!(faces.layer(id, face), PLACEHOLDER_BLOCK_LAYER);
        }
    }

    #[test]
    fn unknown_ids_and_faces_return_the_placeholder_layer() {
        let faces = BlockFaces::builtin();
        // Raw past the built-in table and past the u16->usize arithmetic.
        assert_eq!(
            faces.layer(BlockId::Modded(BUILTIN_BLOCK_COUNT), 0),
            PLACEHOLDER_BLOCK_LAYER
        );
        assert_eq!(
            faces.layer(BlockId::Modded(u16::MAX), 5),
            PLACEHOLDER_BLOCK_LAYER
        );
        // A forged modded payload aliasing a built-in cannot read built-in art.
        assert_eq!(faces.layer(BlockId::Modded(0), 0), PLACEHOLDER_BLOCK_LAYER);
        // Faces 6.. are not directions.
        assert_eq!(faces.layer(BlockId::Stone, 6), PLACEHOLDER_BLOCK_LAYER);
        assert_eq!(
            faces.layer(BlockId::Stone, u8::MAX),
            PLACEHOLDER_BLOCK_LAYER
        );
    }

    #[test]
    fn table_length_is_blocks_times_six_in_raw_id_order() {
        let registry = ContentRegistry::builtin();
        let faces = BlockFaces::from_registry(registry);
        assert_eq!(faces.len(), registry.blocks().len() * 6);
        let mut expected = BTreeMap::new();
        for def in registry.blocks() {
            for face in 0u8..6 {
                expected.insert((def.id.to_raw(), face), def.faces[usize::from(face)]);
            }
        }
        for ((raw, face), layer) in expected {
            assert_eq!(faces.layer(BlockId::from_raw(raw), face), layer);
        }
    }
}
