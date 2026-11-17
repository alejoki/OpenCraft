//! FNV-1a 64 content digest over the neutral catalog definitions.
//!
//! The digest is part of the pack contract: it must be reproducible across
//! processes, platforms and Rust versions, so it is a hand-rolled FNV-1a 64
//! sink over one canonical little-endian byte stream rather than
//! `DefaultHasher`.
//!
//! Only pack definitions enter the digest: pack identities, authored pack
//! entries, their resolved raw ids/indices, pack recipes, worldgen rules and
//! patches. Built-in definitions, asset bytes, resolved texture/icon layers,
//! provenance sources, `[builtin_icons]` and diagnostics are all excluded, so
//! a live texture edit never moves the digest.
//
// The pack-stack builder in `catalog::build` is the in-crate consumer of this
// module; it lands in the next task of this slice. Until it wires
// `compute_digest`, the digest surface is exercised only by this module's
// tests, so keep a dead-code allow scoped to this module rather than letting
// the staged extraction fail `-D warnings`. Drop it when the builder calls in.
#![allow(dead_code)]

use std::collections::BTreeMap;

use crate::spec::{
    BlockEntry, BlockPatch, ItemEntry, PackContentSpec, RecipePatch, TierEntry, ToolEntry, ToolKind,
};
use crate::vocabulary::MATERIAL_ID_NAMES;

use super::{
    BlockDef, ItemDef, ItemRef, PackIdentity, RecipeDef, TierDef, ToolDef, WorldgenRule,
    EMPTY_DIGEST,
};

/// Minimal FNV-1a 64 sink over a canonical little-endian byte stream.
struct Digest(u64);

impl Digest {
    fn new() -> Digest {
        Digest(EMPTY_DIGEST)
    }

    fn finish(self) -> u64 {
        self.0
    }

    fn byte(&mut self, value: u8) {
        self.0 ^= u64::from(value);
        self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
    }

    fn bytes(&mut self, value: &[u8]) {
        for &byte in value {
            self.byte(byte);
        }
    }

    fn u8(&mut self, value: u8) {
        self.byte(value);
    }

    fn u16(&mut self, value: u16) {
        self.bytes(&value.to_le_bytes());
    }

    fn i32(&mut self, value: i32) {
        self.bytes(&value.to_le_bytes());
    }

    fn f32(&mut self, value: f32) {
        self.bytes(&value.to_bits().to_le_bytes());
    }

    fn str(&mut self, value: &str) {
        self.u16(u16::try_from(value.len()).unwrap_or(u16::MAX));
        self.bytes(value.as_bytes());
    }

    fn opt_str(&mut self, value: Option<&str>) {
        match value {
            None => self.u8(0),
            Some(text) => {
                self.u8(1);
                self.str(text);
            }
        }
    }

    fn opt_f32(&mut self, value: Option<f32>) {
        match value {
            None => self.u8(0),
            Some(number) => {
                self.u8(1);
                self.f32(number);
            }
        }
    }

    fn opt_u16(&mut self, value: Option<u16>) {
        match value {
            None => self.u8(0),
            Some(number) => {
                self.u8(1);
                self.u16(number);
            }
        }
    }

    fn opt_tool_kind(&mut self, value: Option<ToolKind>) {
        match value {
            None => self.u8(0),
            Some(kind) => {
                self.u8(1);
                self.u8(tool_kind_tag(kind));
            }
        }
    }

    fn block(&mut self, block: u16) {
        self.u16(block);
    }

    /// Hashes one neutral item reference with the frozen wire tags.
    ///
    /// The encoding is byte-identical to the historical game encoding: a
    /// built-in material hashes `1 + 1 + str(name)`, a built-in tool hashes
    /// `1 + 2 + kind tag + tier tag`, and pack identities keep their own
    /// `2`/`3` outer tags. An unnameable material index (only possible through
    /// a forged neutral ref) hashes an empty name rather than panicking.
    fn item_ref(&mut self, reference: ItemRef) {
        match reference {
            ItemRef::Block(block) => {
                self.u8(0);
                self.block(block);
            }
            ItemRef::BuiltinMaterial(index) => {
                self.u8(1);
                self.u8(1);
                let name = MATERIAL_ID_NAMES
                    .get(usize::from(index))
                    .copied()
                    .unwrap_or("");
                self.str(name);
            }
            ItemRef::BuiltinTool { kind, tier } => {
                self.u8(1);
                self.u8(2);
                self.u8(tool_kind_tag(kind));
                self.u8(tier);
            }
            ItemRef::Item(index) => {
                self.u8(2);
                self.u16(index);
            }
            ItemRef::Tool(index) => {
                self.u8(3);
                self.u16(index);
            }
        }
    }

    fn opt_item_ref(&mut self, reference: Option<ItemRef>) {
        match reference {
            None => self.u8(0),
            Some(reference) => {
                self.u8(1);
                self.item_ref(reference);
            }
        }
    }
}

fn tool_kind_tag(kind: ToolKind) -> u8 {
    match kind {
        ToolKind::Pickaxe => 0,
        ToolKind::Axe => 1,
        ToolKind::Shovel => 2,
        ToolKind::Hoe => 3,
        ToolKind::Sword => 4,
        ToolKind::Bow => 5,
    }
}

/// Everything the deterministic catalog digest folds in.
///
/// The builder's working set (resolved definitions, authored patch lists, and
/// the winning entry map per content kind) travels as one bundle so the digest
/// stays a single entry point as new content kinds appear.
#[derive(Clone, Copy)]
pub(super) struct DigestInputs<'a> {
    pub(super) packs: &'a [(PackIdentity, PackContentSpec)],
    pub(super) blocks: &'a [BlockDef],
    pub(super) items: &'a [ItemDef],
    pub(super) tiers: &'a [TierDef],
    pub(super) tools: &'a [ToolDef],
    pub(super) recipes: &'a [RecipeDef],
    pub(super) builtin_recipe_count: usize,
    pub(super) worldgen_rules: &'a [WorldgenRule],
    pub(super) block_entries: &'a BTreeMap<&'a str, (&'a str, &'a BlockEntry)>,
    pub(super) item_entries: &'a BTreeMap<&'a str, (&'a str, &'a ItemEntry)>,
    pub(super) tier_entries: &'a BTreeMap<&'a str, (&'a str, &'a TierEntry)>,
    pub(super) tool_entries: &'a BTreeMap<&'a str, (&'a str, &'a ToolEntry)>,
    pub(super) block_patches: &'a [(&'a str, &'a BlockPatch)],
    pub(super) recipe_patches: &'a [(&'a str, &'a RecipePatch)],
}

/// Folds the canonical pack-definition byte stream into the FNV-1a digest.
///
/// The field order is frozen: pack identities (folder-sorted) → pack blocks
/// (id-sorted) → items → tiers → tools → pack recipes only → worldgen rules →
/// block patches → recipe patches. An empty pack stack short-circuits to
/// [`EMPTY_DIGEST`] so a rebuilt empty stack always matches a built-in-only
/// catalog.
pub(super) fn compute_digest(input: &DigestInputs<'_>) -> u64 {
    let DigestInputs {
        packs,
        blocks,
        items,
        tiers,
        tools,
        recipes,
        builtin_recipe_count,
        worldgen_rules,
        block_entries,
        item_entries,
        tier_entries,
        tool_entries,
        block_patches,
        recipe_patches,
    } = *input;
    // No packs means no pack definitions: report the empty-stack digest so a
    // rebuilt stack with zero packs matches the built-in catalog exactly.
    if packs.is_empty() {
        return EMPTY_DIGEST;
    }
    let mut digest = Digest::new();

    // Pack identities in folder order.
    let mut identities: Vec<&PackIdentity> = packs.iter().map(|(identity, _)| identity).collect();
    identities.sort_by(|left, right| left.folder.cmp(&right.folder));
    digest.u16(identities.len() as u16);
    for identity in identities {
        digest.str(&identity.folder);
        digest.opt_str(identity.version.as_deref());
    }

    // Pack block definitions in index (sorted id) order, including the
    // authored texture paths so a rename moves the digest even when the
    // allocated layer index is unchanged.
    digest.u16(block_entries.len() as u16);
    for (id, (_folder, entry)) in block_entries {
        let resolved = blocks
            .iter()
            .find(|def| def.name == *id)
            .expect("every collected block has a definition");
        digest.str(id);
        digest.u16(resolved.id);
        digest.opt_str(entry.display_name.as_deref());
        digest.f32(entry.hardness);
        digest.opt_str(entry.drop.as_deref());
        digest.opt_tool_kind(entry.tool);
        digest.u16(entry.stack_limit);
        for face in 0..6 {
            digest.opt_str(face_texture(&entry.textures, face).map(String::as_str));
        }
        digest.opt_item_ref(resolved.drop);
    }

    // Items in index order.
    digest.u16(items.len() as u16);
    for (id, (_folder, entry)) in item_entries {
        let resolved = items
            .iter()
            .find(|def| def.name == *id)
            .expect("every collected item has a definition");
        digest.str(id);
        digest.u16(resolved.index);
        digest.opt_str(entry.display_name.as_deref());
        digest.str(&entry.icon);
        digest.u16(entry.stack_limit);
        match &entry.food {
            None => digest.u8(0),
            Some(food) => {
                digest.u8(1);
                digest.u16(food.nutrition);
                digest.f32(food.eat_seconds);
            }
        }
    }

    // Pack tiers in index order (built-ins are excluded).
    digest.u16(tier_entries.len() as u16);
    for (id, (_folder, entry)) in tier_entries {
        let resolved = tiers
            .iter()
            .find(|def| def.name == *id)
            .expect("every collected tier has a definition");
        digest.str(id);
        digest.u16(resolved.index);
        digest.opt_str(entry.display_name.as_deref());
        digest.f32(entry.speed);
        digest.u16(entry.durability);
        digest.u16(entry.damage);
    }

    // Tools in index order.
    digest.u16(tools.len() as u16);
    for (id, (_folder, entry)) in tool_entries {
        let resolved = tools
            .iter()
            .find(|def| def.name == *id)
            .expect("every collected tool has a definition");
        digest.str(id);
        digest.u16(resolved.index);
        digest.opt_str(entry.display_name.as_deref());
        digest.u8(tool_kind_tag(entry.kind));
        digest.u16(resolved.tier);
        digest.str(&entry.icon);
    }

    // Pack recipes in index order.
    let pack_recipes = &recipes[builtin_recipe_count..];
    digest.u16(pack_recipes.len() as u16);
    for recipe in pack_recipes {
        digest.u16(recipe.index);
        digest.str(&recipe.name);
        digest.u16(recipe.pattern.len() as u16);
        for row in &recipe.pattern {
            digest.str(row);
        }
        digest.u16(recipe.key.len() as u16);
        for (character, reference) in &recipe.key {
            digest.u8(*character as u8);
            digest.item_ref(*reference);
        }
        digest.item_ref(recipe.output);
        digest.u16(recipe.count);
        digest.u8(u8::from(recipe.enabled));
    }

    // Worldgen rules in authored order.
    digest.u16(worldgen_rules.len() as u16);
    for rule in worldgen_rules {
        match rule {
            WorldgenRule::Ore {
                block,
                replace,
                min_y,
                max_y,
                vein,
                attempts,
            } => {
                digest.u8(0);
                digest.block(*block);
                digest.block(*replace);
                digest.i32(*min_y);
                digest.i32(*max_y);
                digest.u8(*vein);
                digest.u8(*attempts);
            }
            WorldgenRule::Patch {
                block,
                replace,
                biomes,
                min_y,
                max_y,
                chance,
            } => {
                digest.u8(1);
                digest.block(*block);
                digest.block(*replace);
                digest.u16(biomes.len() as u16);
                for biome in biomes {
                    digest.u8(biome.index());
                }
                digest.i32(*min_y);
                digest.i32(*max_y);
                digest.f32(*chance);
            }
        }
    }

    // Authored patches in stack order.
    digest.u16(block_patches.len() as u16);
    for (_folder, patch) in block_patches {
        digest.str(&patch.id);
        digest.opt_str(patch.display_name.as_deref());
        digest.opt_f32(patch.hardness);
        digest.opt_str(patch.drop.as_deref());
        digest.opt_tool_kind(patch.tool);
        digest.opt_u16(patch.stack_limit);
    }
    digest.u16(recipe_patches.len() as u16);
    for (_folder, patch) in recipe_patches {
        digest.str(&patch.id);
        digest.u8(u8::from(patch.enabled));
    }

    digest.finish()
}

/// The authored path for one face direction of a block entry.
///
/// Shared with the pack-stack builder (which resolves the same authored paths
/// into texture layers) so the digest and the allocation can never disagree
/// about which face reads which path.
pub(super) fn face_texture(textures: &crate::spec::BlockTextures, face: usize) -> Option<&String> {
    let specific = match face {
        0 | 1 | 4 | 5 => textures.side.as_ref(),
        2 => textures.top.as_ref(),
        3 => textures.bottom.as_ref(),
        _ => None,
    };
    specific.or(textures.all.as_ref())
}

#[cfg(test)]
mod tests {
    use super::super::{BiomeTag, PLACEHOLDER_BLOCK_LAYER};
    use super::*;
    use crate::spec::{BlockTextures, FoodSpec, PackAsset};

    /// Independent FNV-1a 64 implementation over an expected byte vector.
    fn fnv(bytes: &[u8]) -> u64 {
        bytes.iter().fold(EMPTY_DIGEST, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }

    /// Runs one sink sequence and returns the resulting digest.
    fn hash(write: impl FnOnce(&mut Digest)) -> u64 {
        let mut digest = Digest::new();
        write(&mut digest);
        digest.finish()
    }

    fn identity(folder: &str, version: Option<&str>) -> PackIdentity {
        PackIdentity {
            folder: folder.to_string(),
            version: version.map(str::to_string),
        }
    }

    fn block_entry(id: &str, texture: &str) -> BlockEntry {
        BlockEntry {
            id: id.to_string(),
            display_name: None,
            textures: BlockTextures {
                all: Some(texture.to_string()),
                ..BlockTextures::default()
            },
            hardness: 1.0,
            drop: None,
            tool: None,
            stack_limit: 64,
        }
    }

    fn asset(byte: u8) -> PackAsset {
        PackAsset {
            rgba: vec![byte; 1024],
            source_side: 16,
        }
    }

    /// Digest one pack stack without any authored patches or non-block
    /// entries, resolving the block rows the way the builder does.
    fn digest_for(packs: &[(PackIdentity, PackContentSpec)]) -> u64 {
        digest_with(packs, &[], 0)
    }

    fn digest_with(
        packs: &[(PackIdentity, PackContentSpec)],
        recipes: &[RecipeDef],
        builtin_recipe_count: usize,
    ) -> u64 {
        let mut blocks: Vec<BlockDef> = Vec::new();
        let mut block_entries: BTreeMap<&str, (&str, &BlockEntry)> = BTreeMap::new();
        for (pack, spec) in packs {
            for entry in &spec.blocks {
                block_entries.insert(entry.id.as_str(), (pack.folder.as_str(), entry));
            }
        }
        for (id, (_folder, entry)) in &block_entries {
            let raw = u16::try_from(blocks.len()).expect("test block count fits u16") + 24;
            blocks.push(BlockDef {
                id: raw,
                name: (*id).to_string(),
                display_name: entry
                    .display_name
                    .clone()
                    .unwrap_or_else(|| (*id).to_string()),
                faces: [PLACEHOLDER_BLOCK_LAYER; 6],
                hardness: Some(entry.hardness),
                drop: None,
                tool_class: entry.tool,
                stack_limit: entry.stack_limit,
                placeholder: false,
            });
        }
        let item_entries: BTreeMap<&str, (&str, &ItemEntry)> = BTreeMap::new();
        let tier_entries: BTreeMap<&str, (&str, &TierEntry)> = BTreeMap::new();
        let tool_entries: BTreeMap<&str, (&str, &ToolEntry)> = BTreeMap::new();
        let worldgen_rules: Vec<WorldgenRule> = Vec::new();
        let block_patches: Vec<(&str, &BlockPatch)> = Vec::new();
        let recipe_patches: Vec<(&str, &RecipePatch)> = Vec::new();
        let inputs = DigestInputs {
            packs,
            blocks: &blocks,
            items: &[],
            tiers: &[],
            tools: &[],
            recipes,
            builtin_recipe_count,
            worldgen_rules: &worldgen_rules,
            block_entries: &block_entries,
            item_entries: &item_entries,
            tier_entries: &tier_entries,
            tool_entries: &tool_entries,
            block_patches: &block_patches,
            recipe_patches: &recipe_patches,
        };
        compute_digest(&inputs)
    }

    #[test]
    fn digest_sink_hashes_exactly_the_canonical_bytes() {
        assert_eq!(hash(|_| {}), EMPTY_DIGEST);
        assert_eq!(hash(|digest| digest.u8(7)), fnv(&[7]));
        assert_eq!(hash(|digest| digest.u16(0x1234)), fnv(&[0x34, 0x12]));
        assert_eq!(hash(|digest| digest.i32(-2)), fnv(&(-2i32).to_le_bytes()));
        assert_eq!(
            hash(|digest| digest.f32(1.5)),
            fnv(&1.5f32.to_bits().to_le_bytes())
        );
        assert_eq!(hash(|digest| digest.str("ab")), fnv(&[2, 0, b'a', b'b']));
        assert_eq!(
            hash(|digest| digest.str("")),
            fnv(&[0, 0]),
            "an empty string keeps its zero length prefix"
        );

        assert_eq!(hash(|digest| digest.opt_str(None)), fnv(&[0]));
        assert_eq!(
            hash(|digest| digest.opt_str(Some("ab"))),
            fnv(&[1, 2, 0, b'a', b'b'])
        );
        assert_eq!(hash(|digest| digest.opt_f32(None)), fnv(&[0]));
        assert_eq!(
            hash(|digest| digest.opt_f32(Some(0.5))),
            fnv(&[1, 0, 0, 0, 0x3f])
        );
        assert_eq!(hash(|digest| digest.opt_u16(None)), fnv(&[0]));
        assert_eq!(hash(|digest| digest.opt_u16(Some(0x0102))), fnv(&[1, 2, 1]));
        assert_eq!(hash(|digest| digest.opt_tool_kind(None)), fnv(&[0]));
        assert_eq!(
            hash(|digest| digest.opt_tool_kind(Some(ToolKind::Bow))),
            fnv(&[1, 5])
        );

        // The sink is a pure stream: chained writes hash the concatenation.
        assert_eq!(
            hash(|digest| {
                digest.u8(1);
                digest.u16(2);
                digest.str("x");
            }),
            fnv(&[1, 2, 0, 1, 0, b'x'])
        );
    }

    #[test]
    fn item_refs_encode_the_frozen_wire_tags() {
        let cases: [(ItemRef, Vec<u8>); 5] = [
            (ItemRef::Block(0x1234), vec![0, 0x34, 0x12]),
            (
                ItemRef::BuiltinMaterial(0),
                vec![1, 1, 5, 0, b's', b't', b'i', b'c', b'k'],
            ),
            (
                ItemRef::BuiltinTool {
                    kind: ToolKind::Pickaxe,
                    tier: 2,
                },
                vec![1, 2, 0, 2],
            ),
            (ItemRef::Item(0x0102), vec![2, 2, 1]),
            (ItemRef::Tool(3), vec![3, 3, 0]),
        ];
        for (reference, bytes) in cases {
            assert_eq!(
                hash(|digest| digest.item_ref(reference)),
                fnv(&bytes),
                "{reference:?} wire bytes"
            );
        }

        // The last material hashes its frozen name, not its index.
        assert_eq!(
            hash(|digest| digest.item_ref(ItemRef::BuiltinMaterial(10))),
            fnv(&[1, 1, 5, 0, b'a', b'r', b'r', b'o', b'w'])
        );

        // A forged out-of-range material index hashes an empty name instead
        // of panicking on a table lookup.
        assert_eq!(
            hash(|digest| digest.item_ref(ItemRef::BuiltinMaterial(u8::MAX))),
            fnv(&[1, 1, 0, 0])
        );

        assert_eq!(hash(|digest| digest.opt_item_ref(None)), fnv(&[0]));
        assert_eq!(
            hash(|digest| digest.opt_item_ref(Some(ItemRef::Block(1)))),
            fnv(&[1, 0, 1, 0])
        );
    }

    #[test]
    fn tool_kind_tags_cover_the_frozen_order() {
        assert_eq!(
            [
                ToolKind::Pickaxe,
                ToolKind::Axe,
                ToolKind::Shovel,
                ToolKind::Hoe,
                ToolKind::Sword,
                ToolKind::Bow,
            ]
            .map(tool_kind_tag),
            [0, 1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn biome_tags_match_the_vocabulary_order() {
        assert_eq!(
            [
                BiomeTag::Plains,
                BiomeTag::Forest,
                BiomeTag::Mountains,
                BiomeTag::Ocean,
            ]
            .map(BiomeTag::index),
            [0, 1, 2, 3]
        );
    }

    #[test]
    fn empty_pack_stack_is_the_empty_digest() {
        let blocks = vec![BlockDef {
            id: 0,
            name: "stone".to_string(),
            display_name: "Stone".to_string(),
            faces: [1; 6],
            hardness: Some(1.0),
            drop: None,
            tool_class: None,
            stack_limit: 64,
            placeholder: false,
        }];
        let block_entries = BTreeMap::new();
        let item_entries = BTreeMap::new();
        let tier_entries = BTreeMap::new();
        let tool_entries = BTreeMap::new();
        let worldgen_rules = Vec::new();
        let block_patches = Vec::new();
        let recipe_patches = Vec::new();
        let inputs = DigestInputs {
            packs: &[],
            blocks: &blocks,
            items: &[],
            tiers: &[],
            tools: &[],
            recipes: &[],
            builtin_recipe_count: 0,
            worldgen_rules: &worldgen_rules,
            block_entries: &block_entries,
            item_entries: &item_entries,
            tier_entries: &tier_entries,
            tool_entries: &tool_entries,
            block_patches: &block_patches,
            recipe_patches: &recipe_patches,
        };
        assert_eq!(compute_digest(&inputs), EMPTY_DIGEST);
    }

    #[test]
    fn digest_ignores_asset_bytes_and_builtin_icons() {
        let mut first = PackContentSpec::default();
        first
            .blocks
            .push(block_entry("mymod:ruby_ore", "ruby_ore.png"));
        first.assets.insert("ruby_ore.png".to_string(), asset(0x11));

        let mut second = first.clone();
        second
            .assets
            .insert("ruby_ore.png".to_string(), asset(0x22));
        second
            .builtin_icons
            .insert("stick".to_string(), "textures/live_stick.png".to_string());

        let packs = |spec: PackContentSpec| vec![(identity("alpha", Some("1.0")), spec)];
        let first = packs(first);
        let second = packs(second);
        assert_eq!(
            digest_for(&first),
            digest_for(&second),
            "asset bytes and [builtin_icons] are presentation-only"
        );
    }

    #[test]
    fn digest_moves_with_authored_definitions_and_identities() {
        let mut base = PackContentSpec::default();
        base.blocks
            .push(block_entry("mymod:ruby_ore", "ruby_ore.png"));

        let mut edited = base.clone();
        edited.blocks[0].hardness = 2.0;

        let base_stack = vec![(identity("alpha", Some("1.0")), base.clone())];
        let edited_stack = vec![(identity("alpha", Some("1.0")), edited)];
        assert_ne!(digest_for(&base_stack), digest_for(&edited_stack));

        let versioned_stack = vec![(identity("alpha", Some("2.0")), base.clone())];
        assert_ne!(digest_for(&base_stack), digest_for(&versioned_stack));

        let renamed_folder = vec![(identity("beta", Some("1.0")), base.clone())];
        assert_ne!(digest_for(&base_stack), digest_for(&renamed_folder));
    }

    #[test]
    fn digest_sorts_pack_identities_by_folder() {
        let empty = PackContentSpec::default();
        let forward = vec![
            (identity("alpha", Some("1.0")), empty.clone()),
            (identity("beta", Some("2.0")), empty.clone()),
        ];
        let reverse = vec![
            (identity("beta", Some("2.0")), empty.clone()),
            (identity("alpha", Some("1.0")), empty.clone()),
        ];
        assert_eq!(
            digest_for(&forward),
            digest_for(&reverse),
            "stack order is reported separately; identities hash folder-sorted"
        );
    }

    #[test]
    fn digest_hashes_only_pack_recipes() {
        let packs = vec![(identity("alpha", Some("1.0")), PackContentSpec::default())];
        let builtin = RecipeDef {
            index: 0,
            name: "planks".to_string(),
            pattern: vec!["X".to_string()],
            key: BTreeMap::new(),
            output: ItemRef::Block(0),
            count: 4,
            enabled: true,
            placeholder: false,
        };
        let pack_recipe = RecipeDef {
            index: 22,
            name: "mymod:ruby_block".to_string(),
            pattern: vec!["RR".to_string()],
            key: BTreeMap::new(),
            output: ItemRef::Item(0),
            count: 1,
            enabled: true,
            placeholder: false,
        };

        assert_eq!(
            digest_with(&packs, std::slice::from_ref(&builtin), 1),
            digest_with(&packs, &[], 0),
            "built-in recipes are outside the digest"
        );
        assert_ne!(
            digest_with(&packs, &[builtin.clone(), pack_recipe], 1),
            digest_with(&packs, std::slice::from_ref(&builtin), 1),
            "pack recipes are inside the digest"
        );
    }

    #[test]
    fn digest_hashes_pack_recipes_with_neutral_item_refs() {
        let packs = vec![(identity("alpha", Some("1.0")), PackContentSpec::default())];
        let recipe = RecipeDef {
            index: 22,
            name: "mymod:ruby_stew".to_string(),
            pattern: vec!["R".to_string()],
            key: BTreeMap::from([('R', ItemRef::Item(0)), ('S', ItemRef::BuiltinMaterial(0))]),
            output: ItemRef::Item(1),
            count: 1,
            enabled: true,
            placeholder: false,
        };
        let mut disabled = recipe.clone();
        disabled.enabled = false;

        assert_ne!(
            digest_with(&packs, &[recipe], 0),
            digest_with(&packs, &[disabled], 0),
            "the enabled flag is hashed"
        );
    }

    #[test]
    fn digest_hashes_worldgen_rules_with_neutral_biome_tags() {
        let packs = vec![(identity("alpha", Some("1.0")), PackContentSpec::default())];
        let ore = WorldgenRule::Ore {
            block: 24,
            replace: 3,
            min_y: -64,
            max_y: 32,
            vein: 6,
            attempts: 4,
        };
        let patch = WorldgenRule::Patch {
            block: 25,
            replace: 1,
            biomes: vec![BiomeTag::Plains, BiomeTag::Forest],
            min_y: 60,
            max_y: 90,
            chance: 0.2,
        };
        let digest = |rules: &[WorldgenRule]| {
            let block_entries = BTreeMap::new();
            let item_entries = BTreeMap::new();
            let tier_entries = BTreeMap::new();
            let tool_entries = BTreeMap::new();
            let block_patches = Vec::new();
            let recipe_patches = Vec::new();
            let inputs = DigestInputs {
                packs: &packs,
                blocks: &[],
                items: &[],
                tiers: &[],
                tools: &[],
                recipes: &[],
                builtin_recipe_count: 0,
                worldgen_rules: rules,
                block_entries: &block_entries,
                item_entries: &item_entries,
                tier_entries: &tier_entries,
                tool_entries: &tool_entries,
                block_patches: &block_patches,
                recipe_patches: &recipe_patches,
            };
            compute_digest(&inputs)
        };

        let base = digest(&[ore.clone(), patch.clone()]);
        assert_ne!(base, digest(std::slice::from_ref(&ore)));
        let swapped = match patch {
            WorldgenRule::Patch {
                block,
                replace,
                biomes,
                min_y,
                max_y,
                chance,
            } => WorldgenRule::Patch {
                block,
                replace,
                biomes: biomes.into_iter().rev().collect(),
                min_y,
                max_y,
                chance,
            },
            other => other,
        };
        assert_ne!(
            base,
            digest(&[ore, swapped]),
            "biome tag order is part of the digest"
        );
    }

    #[test]
    fn digest_hashes_authored_patches_in_stack_order() {
        let packs = vec![(identity("alpha", Some("1.0")), PackContentSpec::default())];
        let patch = BlockPatch {
            id: "stone".to_string(),
            display_name: Some("Granite".to_string()),
            hardness: Some(2.5),
            drop: Some("cobblestone".to_string()),
            tool: Some(ToolKind::Shovel),
            stack_limit: Some(32),
        };
        let digest = |patch: Option<&BlockPatch>| {
            let block_entries = BTreeMap::new();
            let item_entries = BTreeMap::new();
            let tier_entries = BTreeMap::new();
            let tool_entries = BTreeMap::new();
            let worldgen_rules = Vec::new();
            let block_patches: Vec<(&str, &BlockPatch)> =
                patch.map(|patch| ("alpha", patch)).into_iter().collect();
            let recipe_patches = Vec::new();
            let inputs = DigestInputs {
                packs: &packs,
                blocks: &[],
                items: &[],
                tiers: &[],
                tools: &[],
                recipes: &[],
                builtin_recipe_count: 0,
                worldgen_rules: &worldgen_rules,
                block_entries: &block_entries,
                item_entries: &item_entries,
                tier_entries: &tier_entries,
                tool_entries: &tool_entries,
                block_patches: &block_patches,
                recipe_patches: &recipe_patches,
            };
            compute_digest(&inputs)
        };

        assert_ne!(digest(None), digest(Some(&patch)));
    }

    #[test]
    fn digest_hashes_item_food_bytes() {
        let packs = vec![(identity("alpha", Some("1.0")), PackContentSpec::default())];
        let digest = |food: Option<FoodSpec>| {
            let item = ItemDef {
                index: 0,
                name: "mymod:ruby_stew".to_string(),
                display_name: "Ruby Stew".to_string(),
                icon_layer: 32,
                stack_limit: 16,
                food: food.clone(),
                placeholder: false,
                placeholder_tool: false,
            };
            let block_entries = BTreeMap::new();
            let mut item_entries: BTreeMap<&str, (&str, &ItemEntry)> = BTreeMap::new();
            let entry = ItemEntry {
                id: "mymod:ruby_stew".to_string(),
                display_name: Some("Ruby Stew".to_string()),
                icon: "ruby_stew.png".to_string(),
                stack_limit: 16,
                food,
            };
            item_entries.insert(entry.id.as_str(), ("alpha", &entry));
            let tier_entries = BTreeMap::new();
            let tool_entries = BTreeMap::new();
            let worldgen_rules = Vec::new();
            let block_patches = Vec::new();
            let recipe_patches = Vec::new();
            let inputs = DigestInputs {
                packs: &packs,
                blocks: &[],
                items: std::slice::from_ref(&item),
                tiers: &[],
                tools: &[],
                recipes: &[],
                builtin_recipe_count: 0,
                worldgen_rules: &worldgen_rules,
                block_entries: &block_entries,
                item_entries: &item_entries,
                tier_entries: &tier_entries,
                tool_entries: &tool_entries,
                block_patches: &block_patches,
                recipe_patches: &recipe_patches,
            };
            compute_digest(&inputs)
        };

        let stew = FoodSpec {
            nutrition: 6,
            eat_seconds: 1.6,
        };
        assert_ne!(digest(Some(stew.clone())), digest(None));
        let mut sweeter = stew.clone();
        sweeter.nutrition = 8;
        assert_ne!(digest(Some(stew)), digest(Some(sweeter)));
    }
}
