//! Map-like chunk-edit store with logical copy-on-write (D326).
//!
//! Replaces the eager `FxHashMap<ChunkPos, FxHashMap<u16, BlockId>>` alias:
//! the outer map and every per-chunk [`EditSection`] sit behind an `Arc`, so
//! an autosave snapshot is an `Arc` bump instead of a deep clone and an edit
//! clones only the one section it touches (`Arc::make_mut`).
//!
//! [`EditSection`] keeps the dense representation for edited chunks and adds
//! the palette representation the v10 decoder adopts in T03: a decoded
//! [`ChunkSection`] (sorted `(local, slot)` pairs) plus the palette resolved
//! to block ids. Palette iteration walks the decoded slice; it never rebuilds
//! a hash map and never calls `nth`.
//!
//! This module is structural only: the v10 decoder still builds Dense
//! sections, so the wire bytes and every fail-closed decoder test are
//! unchanged.

use std::collections::hash_map;
use std::fmt;
use std::ops::Index;
use std::slice;
use std::sync::Arc;

use rustc_hash::FxHashMap;

use opencraft_storage::chunk_section::ChunkSection;

use crate::world::block::BlockId;
use crate::world::chunk::ChunkPos;

/// One chunk's edited cells.
///
/// `Dense` is the pre-existing `local -> block` hash map, kept as the
/// representation for edits made in a live session. `Palette` is a decoded
/// v10 section: one `u16` local plus one `u16` palette slot per edit, with
/// the palette resolved once into `blocks`, so 10M edits stay compact.
#[derive(Clone, Debug)]
pub struct EditSection {
    repr: EditSectionRepr,
}

#[derive(Clone, Debug)]
enum EditSectionRepr {
    Dense(FxHashMap<u16, BlockId>),
    Palette {
        section: ChunkSection,
        /// Palette content resolved to block ids, in palette-slot order.
        blocks: Box<[BlockId]>,
    },
}

/// Resolved palette blocks plus sorted `(local, palette_slot)` edits.
pub(crate) type PaletteParts<'a> = (&'a [BlockId], &'a [(u16, u16)]);

impl EditSection {
    /// The block at `local`, or `None` when this cell was never edited.
    ///
    /// Palette sections binary-search the decoded ascending edit list; dense
    /// sections keep the old hash lookup.
    #[inline]
    pub fn get(&self, local: &u16) -> Option<&BlockId> {
        match &self.repr {
            EditSectionRepr::Dense(blocks) => blocks.get(local),
            EditSectionRepr::Palette { section, blocks } => {
                let index = section
                    .edits()
                    .binary_search_by_key(local, |&(edit_local, _)| edit_local)
                    .ok()?;
                let slot = section.edits()[index].1;
                blocks.get(usize::from(slot))
            }
        }
    }

    /// Number of edited cells in this section.
    #[inline]
    pub fn len(&self) -> usize {
        match &self.repr {
            EditSectionRepr::Dense(blocks) => blocks.len(),
            EditSectionRepr::Palette { section, .. } => section.len(),
        }
    }

    /// Whether this section carries no edits.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether this section is stored in the compact palette representation.
    #[inline]
    pub fn is_palette(&self) -> bool {
        matches!(self.repr, EditSectionRepr::Palette { .. })
    }

    /// Records `block` at `local`.
    ///
    /// A palette section materializes into a dense map first: only this
    /// section converts, and every existing edit is preserved.
    pub fn insert(&mut self, local: u16, block: BlockId) {
        match &mut self.repr {
            EditSectionRepr::Dense(blocks) => {
                blocks.insert(local, block);
            }
            EditSectionRepr::Palette { section, blocks } => {
                let mut dense =
                    FxHashMap::with_capacity_and_hasher(section.len() + 1, Default::default());
                for &(edit_local, slot) in section.edits() {
                    dense.insert(edit_local, blocks[usize::from(slot)]);
                }
                dense.insert(local, block);
                self.repr = EditSectionRepr::Dense(dense);
            }
        }
    }

    /// Iterates every edit as `(local, block)`.
    ///
    /// Dense sections yield the hash-map iterator; palette sections zip the
    /// decoded edit slice with its resolved palette, so each step is O(1).
    pub fn iter(&self) -> EditSectionIter<'_> {
        let inner = match &self.repr {
            EditSectionRepr::Dense(blocks) => EditSectionIterRepr::Dense(blocks.iter()),
            EditSectionRepr::Palette { section, blocks } => EditSectionIterRepr::Palette {
                edits: section.edits().iter(),
                blocks,
            },
        };
        EditSectionIter { inner }
    }

    /// Iterates every edited block value.
    pub fn values(&self) -> impl Iterator<Item = &BlockId> + '_ {
        self.iter().map(|(_, block)| block)
    }

    /// The T03 decode seam: the resolved palette and the sorted
    /// `(local, palette_slot)` edits of a palette section.
    ///
    /// `None` for a dense section. T03's decoder consumes this to re-encode
    /// sections without materializing a hash map; until then the unit tests
    /// below are the only in-crate consumers.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn palette_parts(&self) -> Option<PaletteParts<'_>> {
        match &self.repr {
            EditSectionRepr::Dense(_) => None,
            EditSectionRepr::Palette { section, blocks } => {
                Some((blocks.as_ref(), section.edits()))
            }
        }
    }
}

impl Default for EditSection {
    fn default() -> Self {
        Self {
            repr: EditSectionRepr::Dense(FxHashMap::default()),
        }
    }
}

impl PartialEq for EditSection {
    /// Logical equality: same length and the same `local -> block` mapping,
    /// independent of dense/palette representation.
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .all(|(local, block)| other.get(local) == Some(block))
    }
}

impl Index<&u16> for EditSection {
    type Output = BlockId;

    fn index(&self, local: &u16) -> &Self::Output {
        self.get(local)
            .unwrap_or_else(|| panic!("chunk edit at local {local} is missing"))
    }
}

impl<'a> IntoIterator for &'a EditSection {
    type Item = (&'a u16, &'a BlockId);
    type IntoIter = EditSectionIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over an [`EditSection`]'s `(local, block)` edits.
pub struct EditSectionIter<'a> {
    inner: EditSectionIterRepr<'a>,
}

enum EditSectionIterRepr<'a> {
    Dense(hash_map::Iter<'a, u16, BlockId>),
    Palette {
        edits: slice::Iter<'a, (u16, u16)>,
        blocks: &'a [BlockId],
    },
}

impl<'a> Iterator for EditSectionIter<'a> {
    type Item = (&'a u16, &'a BlockId);

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            EditSectionIterRepr::Dense(iter) => iter.next(),
            EditSectionIterRepr::Palette { edits, blocks } => {
                let (local, slot) = edits.next()?;
                let blocks: &'a [BlockId] = blocks;
                Some((local, &blocks[usize::from(*slot)]))
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.inner {
            EditSectionIterRepr::Dense(iter) => iter.size_hint(),
            EditSectionIterRepr::Palette { edits, .. } => edits.size_hint(),
        }
    }
}

/// Map-like store of every chunk edit in a world.
///
/// Logical surface mirrors the old `FxHashMap` alias (`get`, `contains_key`,
/// `len`, `is_empty`, `iter`, `values`, `insert`, `Index`) so call sites keep
/// reading like a map, while `Clone` is O(1) and mutation is copy-on-write.
#[derive(Clone, Default)]
pub struct ChunkEditMap {
    sections: Arc<FxHashMap<ChunkPos, Arc<EditSection>>>,
}

impl ChunkEditMap {
    /// An empty map with room for `capacity` chunks.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            sections: Arc::new(FxHashMap::with_capacity_and_hasher(
                capacity,
                Default::default(),
            )),
        }
    }

    /// The section at `pos`, including an empty one.
    #[inline]
    pub fn get(&self, pos: &ChunkPos) -> Option<&EditSection> {
        self.sections.get(pos).map(|section| &**section)
    }

    /// Whether `pos` has a recorded section, even an empty one.
    #[inline]
    pub fn contains_key(&self, pos: &ChunkPos) -> bool {
        self.sections.contains_key(pos)
    }

    /// Number of chunks with a recorded section.
    #[inline]
    pub fn len(&self) -> usize {
        self.sections.len()
    }

    /// Whether no chunk section is recorded.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    /// Iterates every `(chunk, section)` pair.
    pub fn iter(&self) -> ChunkEditMapIter<'_> {
        ChunkEditMapIter(self.sections.iter())
    }

    /// Iterates every recorded section.
    pub fn values(&self) -> impl Iterator<Item = &EditSection> + '_ {
        self.sections.values().map(|section| &**section)
    }

    /// Replaces the section at `pos` with a dense map.
    ///
    /// An empty map is retained as an empty Dense section, matching the old
    /// alias (the writer, not the store, is what drops empty chunks).
    pub fn insert(&mut self, pos: ChunkPos, edits: FxHashMap<u16, BlockId>) {
        Arc::make_mut(&mut self.sections).insert(
            pos,
            Arc::new(EditSection {
                repr: EditSectionRepr::Dense(edits),
            }),
        );
    }

    /// Records one block edit, creating the chunk's Dense section on demand.
    ///
    /// COW at both levels: `Arc::make_mut` clones the outer map only when a
    /// snapshot still shares it, and clones only the edited section.
    pub fn insert_block(&mut self, pos: ChunkPos, local: u16, block: BlockId) {
        let sections = Arc::make_mut(&mut self.sections);
        match sections.get_mut(&pos) {
            Some(section) => Arc::make_mut(section).insert(local, block),
            None => {
                let mut section = EditSection::default();
                section.insert(local, block);
                sections.insert(pos, Arc::new(section));
            }
        }
    }

    /// The T03 decode seam: stores a decoded palette section without
    /// materializing a dense map.
    ///
    /// `blocks` must be `section.palette()` resolved to block ids in the same
    /// slot order (the decoder builds it that way). Until T03 switches the v10
    /// decoder to this path, the unit tests below are the only consumers.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn insert_palette(
        &mut self,
        pos: ChunkPos,
        section: ChunkSection,
        blocks: Box<[BlockId]>,
    ) {
        debug_assert_eq!(
            blocks.len(),
            section.palette().len(),
            "palette blocks must align with the decoded palette"
        );
        Arc::make_mut(&mut self.sections).insert(
            pos,
            Arc::new(EditSection {
                repr: EditSectionRepr::Palette { section, blocks },
            }),
        );
    }
}

impl fmt::Debug for ChunkEditMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl PartialEq for ChunkEditMap {
    /// Logical equality: same chunk set and logically equal sections,
    /// independent of representation.
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .sections
                .iter()
                .all(|(pos, section)| other.get(pos) == Some(&**section))
    }
}

impl Index<&ChunkPos> for ChunkEditMap {
    type Output = EditSection;

    fn index(&self, pos: &ChunkPos) -> &Self::Output {
        self.get(pos)
            .unwrap_or_else(|| panic!("chunk edit section for {pos:?} is missing"))
    }
}

impl<'a> IntoIterator for &'a ChunkEditMap {
    type Item = (&'a ChunkPos, &'a EditSection);
    type IntoIter = ChunkEditMapIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over a [`ChunkEditMap`]'s `(chunk, section)` pairs.
pub struct ChunkEditMapIter<'a>(hash_map::Iter<'a, ChunkPos, Arc<EditSection>>);

impl<'a> Iterator for ChunkEditMapIter<'a> {
    type Item = (&'a ChunkPos, &'a EditSection);

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(pos, section)| (pos, &**section))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use opencraft_storage::chunk_section::encode_chunk_section;

    /// Build a decoded palette section plus its resolved block palette from
    /// logical `(local, block)` edits, exactly as the T03 decoder will.
    fn decoded_palette(edits: &[(u16, BlockId)]) -> (ChunkSection, Box<[BlockId]>) {
        let raw: Vec<(u16, u16)> = edits
            .iter()
            .map(|&(local, block)| (local, block.to_raw()))
            .collect();
        let mut bytes = Vec::new();
        encode_chunk_section(&mut bytes, &raw).expect("test edits are encodable");
        let section = ChunkSection::decode(&bytes).expect("encoded section decodes");
        let blocks: Box<[BlockId]> = section
            .palette()
            .iter()
            .map(|&raw| BlockId::from_raw(raw))
            .collect();
        (section, blocks)
    }

    #[test]
    fn dense_section_supports_get_len_iter_values_and_index() {
        let mut section = EditSection::default();
        assert!(section.is_empty());
        assert_eq!(section.len(), 0);
        assert!(!section.is_palette());
        assert_eq!(section.get(&3), None);
        assert_eq!(section.iter().count(), 0);
        assert_eq!(section.values().count(), 0);

        section.insert(3, BlockId::Stone);
        section.insert(70, BlockId::Brick);

        assert_eq!(section.len(), 2);
        assert!(!section.is_empty());
        assert!(!section.is_palette());
        assert_eq!(section.get(&3), Some(&BlockId::Stone));
        assert_eq!(section.get(&70), Some(&BlockId::Brick));
        assert_eq!(section.get(&4), None);
        assert_eq!(section[&3], BlockId::Stone);
        assert_eq!(section[&70], BlockId::Brick);

        let pairs: FxHashMap<u16, BlockId> =
            (&section).into_iter().map(|(&l, &b)| (l, b)).collect();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[&3], BlockId::Stone);
        assert_eq!(pairs[&70], BlockId::Brick);

        let mut values: Vec<BlockId> = section.values().copied().collect();
        values.sort_by_key(|block| block.to_raw());
        assert_eq!(values, vec![BlockId::Stone, BlockId::Brick]);
    }

    #[test]
    fn empty_dense_section_is_retained_by_the_map() {
        let pos = ChunkPos::new(-2, 0, 3);
        let mut map = ChunkEditMap::default();
        map.insert(pos, FxHashMap::default());

        assert_eq!(map.len(), 1);
        assert!(!map.is_empty());
        assert!(map.contains_key(&pos));
        assert!(map.get(&pos).expect("empty section is retained").is_empty());
        assert_eq!(map.values().count(), 1);
        assert_eq!((&map).into_iter().count(), 1);
        assert_eq!(map.iter().next().map(|(p, _)| *p), Some(pos));
        assert_eq!(&map[&pos], &EditSection::default());
        assert!(!map.contains_key(&ChunkPos::new(0, 0, 0)));

        let mut other = ChunkEditMap::default();
        other.insert(pos, FxHashMap::default());
        assert_eq!(map, other);
    }

    #[test]
    fn clone_snapshot_is_isolated_from_later_edits() {
        let chunk = ChunkPos::new(1, 2, 3);
        let other = ChunkPos::new(4, 5, 6);
        let mut edits = ChunkEditMap::default();
        edits.insert_block(chunk, 7, BlockId::Stone);

        let snapshot = edits.clone();
        edits.insert_block(chunk, 7, BlockId::Brick);
        edits.insert_block(other, 1, BlockId::Dirt);

        assert_eq!(snapshot[&chunk][&7], BlockId::Stone);
        assert_eq!(snapshot.len(), 1);
        assert!(!snapshot.contains_key(&other));

        assert_eq!(edits[&chunk][&7], BlockId::Brick);
        assert_eq!(edits[&other][&1], BlockId::Dirt);
        assert_eq!(edits.len(), 2);

        drop(edits);
        assert_eq!(snapshot[&chunk][&7], BlockId::Stone);
    }

    #[test]
    fn palette_section_supports_the_same_logical_surface() {
        let chunk = ChunkPos::new(0, -1, 2);
        let (section, blocks) = decoded_palette(&[(4, BlockId::Brick), (9, BlockId::Stone)]);
        let mut map = ChunkEditMap::default();
        map.insert_palette(chunk, section, blocks);

        let entry = map.get(&chunk).expect("palette section is present");
        assert!(entry.is_palette());
        assert_eq!(entry.len(), 2);
        assert!(!entry.is_empty());
        assert_eq!(entry.get(&4), Some(&BlockId::Brick));
        assert_eq!(entry.get(&9), Some(&BlockId::Stone));
        assert_eq!(entry.get(&5), None);
        assert_eq!(entry[&4], BlockId::Brick);
        assert_eq!(entry[&9], BlockId::Stone);

        let pairs: Vec<(u16, BlockId)> = entry
            .iter()
            .map(|(&local, &block)| (local, block))
            .collect();
        assert_eq!(pairs, vec![(4, BlockId::Brick), (9, BlockId::Stone)]);

        let values: Vec<BlockId> = entry.values().copied().collect();
        assert_eq!(values, vec![BlockId::Brick, BlockId::Stone]);
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn palette_parts_reports_blocks_and_sorted_edits() {
        // Brick (raw 9) and Stone (raw 3): the codec sorts the palette by
        // content index, so slot 0 is Stone and slot 1 is Brick.
        let (section, blocks) = decoded_palette(&[(4, BlockId::Brick), (9, BlockId::Stone)]);
        let mut map = ChunkEditMap::default();
        map.insert_palette(ChunkPos::new(0, 0, 0), section, blocks);

        let entry = map.get(&ChunkPos::new(0, 0, 0)).expect("section present");
        let (blocks, edits) = entry.palette_parts().expect("palette parts");
        assert_eq!(blocks, [BlockId::Stone, BlockId::Brick]);
        assert_eq!(edits, [(4, 1), (9, 0)]);
        assert!(edits[0].0 < edits[1].0, "edits stay ascending");
    }

    #[test]
    fn insert_into_a_palette_section_converts_only_that_section() {
        let palette_chunk = ChunkPos::new(0, 0, 0);
        let dense_chunk = ChunkPos::new(1, 0, 0);
        let (section, blocks) = decoded_palette(&[(4, BlockId::Brick), (9, BlockId::Stone)]);
        let mut map = ChunkEditMap::default();
        map.insert_palette(palette_chunk, section, blocks);
        map.insert_block(dense_chunk, 2, BlockId::Dirt);

        map.insert_block(palette_chunk, 20, BlockId::Air);

        let converted = &map[&palette_chunk];
        assert!(!converted.is_palette());
        assert_eq!(converted.len(), 3);
        assert_eq!(converted.get(&4), Some(&BlockId::Brick));
        assert_eq!(converted.get(&9), Some(&BlockId::Stone));
        assert_eq!(converted.get(&20), Some(&BlockId::Air));
        assert_eq!(converted.palette_parts(), None);

        // A dense overwrite keeps the count and replaces only that cell.
        map.insert_block(palette_chunk, 4, BlockId::Sand);
        assert_eq!(map[&palette_chunk].len(), 3);
        assert_eq!(map[&palette_chunk][&4], BlockId::Sand);

        let untouched = &map[&dense_chunk];
        assert_eq!(untouched.len(), 1);
        assert!(!untouched.is_palette());
        assert_eq!(untouched.get(&2), Some(&BlockId::Dirt));
    }

    #[test]
    fn palette_and_dense_sections_with_the_same_edits_are_equal() {
        let pos = ChunkPos::new(3, 3, 3);
        let logical = [(4u16, BlockId::Brick), (9, BlockId::Stone)];
        let (section, blocks) = decoded_palette(&logical);
        let mut palette_map = ChunkEditMap::default();
        palette_map.insert_palette(pos, section, blocks);

        let mut dense_map = ChunkEditMap::default();
        for (local, block) in logical {
            dense_map.insert_block(pos, local, block);
        }

        assert_eq!(palette_map[&pos], dense_map[&pos]);
        assert_eq!(dense_map[&pos], palette_map[&pos]);
        assert_eq!(palette_map, dense_map);
        assert_eq!(dense_map, palette_map);

        let mut different = dense_map.clone();
        different.insert_block(pos, 4, BlockId::Sand);
        assert_ne!(palette_map, different);
        assert_ne!(palette_map[&pos], different[&pos]);

        let mut shorter = dense_map.clone();
        shorter.insert(pos, FxHashMap::default());
        assert_ne!(palette_map, shorter);
    }

    #[test]
    fn with_capacity_default_clone_and_debug_are_consistent() {
        let empty = ChunkEditMap::with_capacity(128);
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert_eq!(empty.iter().count(), 0);
        assert_eq!(empty.values().count(), 0);
        assert_eq!(empty, ChunkEditMap::default());
        assert!(format!("{empty:?}").contains("{}"));

        let mut map = ChunkEditMap::with_capacity(4);
        map.insert_block(ChunkPos::new(0, 0, 0), 1, BlockId::Stone);
        let clone = map.clone();
        assert_eq!(clone, map);
        assert_eq!(clone.len(), 1);
        assert!(format!("{map:?}").contains("ChunkPos"));
        assert!(format!("{:?}", map[&ChunkPos::new(0, 0, 0)]).contains("Dense"));
        assert!(format!("{:?}", EditSection::default()).contains("Dense"));
    }

    #[test]
    #[should_panic(expected = "chunk edit section")]
    fn index_panics_for_a_missing_section() {
        let map = ChunkEditMap::default();
        let _ = &map[&ChunkPos::new(0, 0, 0)];
    }

    #[test]
    #[should_panic(expected = "chunk edit at local")]
    fn index_panics_for_a_missing_local() {
        let mut section = EditSection::default();
        section.insert(1, BlockId::Stone);
        let _ = section[&2];
    }
}
