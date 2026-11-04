//! Immutable content registry and built-in content definitions.
//!
//! [`ContentRegistry::builtin`] is the single authoritative built-in content
//! table. [`registry::build_registry`] folds an ordered pack stack over it into
//! one deterministic, immutable [`ContentRegistry`]. Later slices freeze the
//! result behind `Arc<ContentRegistry>` once modded content is live.
//!
//! This module deliberately has no render, audio, settings, filesystem or pack
//! dependency: content identity must stay decoupled from presentation and
//! persistence so the pack parser (S02) can build a registry without touching
//! the GPU or the save layer. It may import *pure* player rules (inventory,
//! tools) and world enums (block ids, biomes).

pub mod faces;
pub mod names;
pub mod registry;
pub mod spec;

pub use registry::{build_registry, ContentLoadReport, PackIdentity};
pub use spec::{ContentDiagnostic, PackContentSpec, Severity};

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use crate::player::inventory::{
    self, Item, ItemId, Recipe, ToolKind, ToolTier, MATERIAL_STACK_LIMIT, MAX_STACK_COUNT, RECIPES,
};
use crate::player::tools;
use crate::world::block::{BlockId, BUILTIN_BLOCK_COUNT};
use crate::world::terrain::Biome;

/// First texture-array layer reserved for pack-authored block faces.
///
/// Mirrors [`crate::render::texture::NUM_TEX_LAYERS`] as a literal so `content`
/// stays render-free; S04 grows the array to [`PACK_LAYER_CAP`].
pub const PACK_LAYER_BASE: u8 = 48;

/// Exclusive upper bound for pack-authored block face layers (160 pack layers).
pub const PACK_LAYER_CAP: u8 = 208;

/// First reserved block face layer used when a pack asset is missing or the
/// layer space overflows: the built-in checker placeholder.
pub const PLACEHOLDER_BLOCK_LAYER: u8 = 25;

/// First icon layer reserved for pack-authored item/tool icons.
pub const PACK_ICON_BASE: u8 = 32;

/// Exclusive upper bound for pack-authored icon layers (192 pack icons).
pub const PACK_ICON_CAP: u8 = 224;

/// Reserved icon layer used when a pack icon is missing or icon space overflows.
pub const PLACEHOLDER_ICON_LAYER: u8 = 27;

/// Digest of a registry with no packs: the FNV-1a 64 offset basis.
///
/// The digest excludes built-in definitions, asset bytes and diagnostics, so
/// the built-in registry is exactly the empty-stack digest.
pub const EMPTY_DIGEST: u64 = 0xcbf2_9ce4_8422_2325;

/// A reference to an item identity before `ItemId`/`RecipeId` widening.
///
/// Built-in identities reuse the frozen [`ItemId`] vocabulary; pack identities
/// stay dense indices into the registry tables so S03+ can widen `ItemId`
/// without a spec change.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ItemRef {
    /// A block identity, built-in or registry-allocated.
    Block(BlockId),
    /// A built-in non-block item (material or tool).
    Builtin(ItemId),
    /// Registry-allocated pack item index.
    Item(u16),
    /// Registry-allocated pack tool index.
    Tool(u16),
}

impl ItemRef {
    /// Classify a built-in [`ItemId`]: blocks stay [`ItemRef::Block`], every
    /// other built-in identity becomes [`ItemRef::Builtin`].
    pub const fn from_item_id(id: ItemId) -> ItemRef {
        match id {
            ItemId::Block(block) => ItemRef::Block(block),
            other => ItemRef::Builtin(other),
        }
    }

    /// The insertable [`ItemId`] for this reference.
    pub const fn item_id(self) -> Option<ItemId> {
        match self {
            ItemRef::Block(block) => Some(ItemId::Block(block)),
            ItemRef::Builtin(id) => Some(id),
            ItemRef::Item(index) => Some(ItemId::Item(index)),
            ItemRef::Tool(index) => Some(ItemId::ModTool(index)),
        }
    }
}

impl ContentRegistry {
    /// Resolve an authored [`ItemRef`] to an insertable [`ItemId`].
    ///
    /// This is the fail-closed bridge used by recipe matching: an item or tool
    /// index that is absent from the registry, or points at placeholder
    /// content, resolves to `None` so a malformed recipe can never match. A
    /// pack block placeholder likewise never matches. Built-in identities are
    /// always resolvable because the built-in tables are frozen and complete.
    pub fn resolve_item_ref(&self, reference: ItemRef) -> Option<ItemId> {
        match reference {
            ItemRef::Block(block) => self
                .block(block)
                .filter(|def| !def.placeholder)
                .map(|_| ItemId::Block(block)),
            ItemRef::Builtin(id) => Some(id),
            ItemRef::Item(index) => self
                .item(index)
                .filter(|def| !def.placeholder)
                .map(|_| ItemId::Item(index)),
            ItemRef::Tool(index) => self.tool(index).map(|_| ItemId::ModTool(index)),
        }
    }
}

/// Food behaviour authored on a pack item.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FoodDef {
    /// Hunger points restored, 1..=20.
    pub nutrition: u16,
    /// Seconds the eat animation takes, 0.1..=10.0.
    pub eat_seconds: f32,
}

/// A resolved generic item definition.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemDef {
    /// Dense registry index (`ItemRef::Item`).
    pub index: u16,
    /// Namespaced stable id (for example `mymod:ruby`).
    pub name: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Reported icon layer; index into [`ContentRegistry::pack_icons`] as
    /// `icon_layer - PACK_ICON_BASE`.
    pub icon_layer: u8,
    /// Maximum stack size.
    pub stack_limit: u16,
    /// Food behaviour, when the item is edible.
    pub food: Option<FoodDef>,
    /// Whether this definition is placeholder art awaiting pack content.
    pub placeholder: bool,
    /// Whether this placeholder stands in for a pack tool whose pack is
    /// missing.
    ///
    /// Only `true` for a tool degraded by an absent pack. Persistence uses it
    /// to re-save the record as a pack-tool tag so that restoring the pack
    /// resolves the same wire name back to the live tool instead of failing
    /// on a cross-kind generic-item reference.
    pub placeholder_tool: bool,
}

/// A resolved tool tier definition.
#[derive(Clone, Debug, PartialEq)]
pub struct TierDef {
    /// Dense registry index; built-ins occupy `0..=2`.
    pub index: u16,
    /// Short stable name (`wood`, `mymod:ruby`).
    pub name: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Speed multiplier applied to a matched block's base hardness.
    pub speed: f32,
    /// Maximum durability.
    pub durability: u16,
    /// Sword damage at this tier.
    pub damage: u16,
}

/// A resolved tool definition.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolDef {
    /// Dense registry index (`ItemRef::Tool`).
    pub index: u16,
    /// Namespaced stable id (for example `mymod:ruby_pickaxe`).
    pub name: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Tool behaviour class.
    pub kind: ToolKind,
    /// Index into [`ContentRegistry::tiers`].
    pub tier: u16,
    /// Reported icon layer; index into [`ContentRegistry::pack_icons`] as
    /// `icon_layer - PACK_ICON_BASE`.
    pub icon_layer: u8,
}

/// A resolved shaped recipe definition.
#[derive(Clone, Debug, PartialEq)]
pub struct RecipeDef {
    /// Dense registry index; built-ins occupy `0..22`.
    pub index: u16,
    /// Stable name (`planks` for a built-in, a namespaced id for a pack).
    pub name: String,
    /// Shaped pattern rows; spaces are empty cells.
    pub pattern: Vec<String>,
    /// Pattern character to item reference. Space is never a key.
    pub key: BTreeMap<char, ItemRef>,
    /// Crafted item.
    pub output: ItemRef,
    /// Crafted count.
    pub count: u16,
    /// Whether the recipe is currently craftable (patches may disable it).
    pub enabled: bool,
    /// Whether this definition is placeholder content awaiting a pack.
    pub placeholder: bool,
}

/// A resolved worldgen rule, in authored stack order.
#[derive(Clone, Debug, PartialEq)]
pub enum WorldgenRule {
    /// Ore vein replacing a host block below the surface.
    Ore {
        block: BlockId,
        replace: BlockId,
        min_y: i32,
        max_y: i32,
        vein: u8,
        attempts: u8,
    },
    /// Surface patch replacing a host block inside listed biomes.
    Patch {
        block: BlockId,
        replace: BlockId,
        biomes: Vec<Biome>,
        min_y: i32,
        max_y: i32,
        chance: f32,
    },
}

/// A resolved block definition.
///
/// Built-in definitions are derived from the existing [`BlockId`] accessors so
/// "registry equals built-ins" holds by construction.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockDef {
    /// Stable identity this definition describes.
    pub id: BlockId,
    /// Unqualified stable name used by packs and saves (for example `oak_log`).
    pub name: String,
    /// Human-readable display name (for example `Oak Log`).
    pub display_name: String,
    /// Texture-array layer per face direction: +X, -X, +Y, -Y, +Z, -Z.
    ///
    /// Pack-authored layers are reported as `PACK_LAYER_BASE + index`.
    pub faces: [u8; 6],
    /// Seconds of held left-click needed to break, or `None` if unbreakable.
    pub hardness: Option<f32>,
    /// Item produced on break, or `None` for uncarryable blocks.
    pub drop: Option<ItemRef>,
    /// Tool class that efficiently breaks this block, if any.
    pub tool_class: Option<ToolKind>,
    /// Maximum stack size of the block item (0 when there is no drop).
    pub stack_limit: u16,
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
        let dropped = id.mined_drop();
        let drop = if hardness.is_some() && dropped.is_carryable() {
            Some(ItemRef::Block(dropped))
        } else {
            None
        };
        let stack_limit = if drop.is_some() { MAX_STACK_COUNT } else { 0 };
        Some(BlockDef {
            id,
            name,
            display_name,
            faces,
            hardness,
            drop,
            tool_class: tools::block_tool_class(id),
            stack_limit,
            placeholder: false,
        })
    }
}

/// Immutable, session-scoped registry of content definitions.
///
/// The built-in registry is created once and shared through a `OnceLock`; a
/// pack stack is folded over it by [`registry::build_registry`]. Allocation is
/// deterministic (sorted ids, dense indices, append-only layers/icons), so a
/// `--validate-pack` run and a game load agree on ids, layers and digest.
#[derive(Debug, Clone)]
pub struct ContentRegistry {
    blocks: Vec<BlockDef>,
    items: Vec<ItemDef>,
    tiers: Vec<TierDef>,
    tools: Vec<ToolDef>,
    recipes: Vec<RecipeDef>,
    worldgen_rules: Vec<WorldgenRule>,
    pack_layers: Vec<Vec<u8>>,
    pack_icons: Vec<Vec<u8>>,
    packs: Vec<PackIdentity>,
    digest: u64,
}

impl ContentRegistry {
    /// The immutable built-in registry, built once per process.
    pub fn builtin() -> &'static ContentRegistry {
        static REGISTRY: OnceLock<ContentRegistry> = OnceLock::new();
        REGISTRY.get_or_init(|| ContentRegistry {
            blocks: builtin_blocks(),
            items: Vec::new(),
            tiers: builtin_tiers(),
            tools: Vec::new(),
            recipes: builtin_recipes(),
            worldgen_rules: Vec::new(),
            pack_layers: Vec::new(),
            pack_icons: Vec::new(),
            packs: Vec::new(),
            digest: EMPTY_DIGEST,
        })
    }

    /// The built-in registry behind a shared `Arc`, built once per process.
    ///
    /// Callers that need an owned, mutable episode registry clone this value
    /// instead of rebuilding the built-in tables.
    pub fn builtin_arc() -> Arc<ContentRegistry> {
        static REGISTRY: OnceLock<Arc<ContentRegistry>> = OnceLock::new();
        Arc::clone(REGISTRY.get_or_init(|| Arc::new(ContentRegistry::builtin().clone())))
    }

    /// Appends a deterministic placeholder block for a save-local name.
    ///
    /// The new definition's raw id is the current table length, which is
    /// always at least [`BUILTIN_BLOCK_COUNT`], so it can never alias a
    /// built-in id. The placeholder uses the reserved checker face layer and
    /// has no drop. Returns the allocated id.
    pub fn append_placeholder_block(&mut self, name: &str) -> BlockId {
        let raw = u16::try_from(self.blocks.len())
            .expect("content registry block count exceeds the u16 id space");
        let id = BlockId::Modded(raw);
        self.blocks.push(BlockDef {
            id,
            name: name.to_string(),
            display_name: name.to_string(),
            faces: [PLACEHOLDER_BLOCK_LAYER; 6],
            hardness: None,
            drop: None,
            tool_class: None,
            stack_limit: MAX_STACK_COUNT,
            placeholder: true,
        });
        id
    }

    /// Appends a deterministic placeholder item for a save-local name.
    ///
    /// The new definition's dense index is the current table length. The
    /// placeholder uses the reserved transparent icon layer and is not food.
    /// Returns the allocated index.
    pub fn append_placeholder_item(&mut self, name: &str) -> u16 {
        let index = u16::try_from(self.items.len())
            .expect("content registry item count exceeds the u16 item space");
        self.items.push(ItemDef {
            index,
            name: name.to_string(),
            display_name: name.to_string(),
            icon_layer: PLACEHOLDER_ICON_LAYER,
            stack_limit: MAX_STACK_COUNT,
            food: None,
            placeholder: true,
            placeholder_tool: false,
        });
        index
    }

    /// Appends a deterministic placeholder for a pack tool whose pack is gone.
    ///
    /// Behaves like [`Self::append_placeholder_item`] but records
    /// `placeholder_tool`, so a re-save keeps the record on the pack-tool wire
    /// tag and a later load with the restored pack resolves the live tool
    /// again. Tools never stack, so the placeholder is count-one only.
    ///
    /// Persistence policy: wear is not carried through the placeholder — the
    /// restored tool comes back at its authored maximum durability.
    pub fn append_placeholder_tool_item(&mut self, name: &str) -> u16 {
        let index = self.append_placeholder_item(name);
        let def = self
            .items
            .get_mut(usize::from(index))
            .expect("the placeholder was just appended");
        def.placeholder_tool = true;
        def.stack_limit = 1;
        index
    }

    /// The saved name of `id`, or `None` when the id is unknown.
    pub fn block_name(&self, id: BlockId) -> Option<&str> {
        self.block(id).map(|def| def.name.as_str())
    }

    /// The saved name of the generic item at dense `index`.
    pub fn item_def_name(&self, index: u16) -> Option<&str> {
        self.item(index).map(|def| def.name.as_str())
    }

    /// The saved name of the pack tool at dense `index`.
    pub fn tool_def_name(&self, index: u16) -> Option<&str> {
        self.tool(index).map(|def| def.name.as_str())
    }

    /// The display name for `id`, resolved through the registry where the
    /// identity is registry-allocated.
    ///
    /// Blocks, generic items and pack tools read their definition's authored
    /// display name. Materials and built-in tools have static `ItemId` labels
    /// and fall back to [`ItemId::name`]. Unknown registry indices return
    /// `None` rather than panicking on forged content.
    pub fn item_display_name(&self, id: ItemId) -> Option<String> {
        match id {
            ItemId::Block(block) => self.block(block).map(|def| def.display_name.clone()),
            ItemId::Item(index) => self.item(index).map(|def| def.display_name.clone()),
            ItemId::ModTool(index) => self.tool(index).map(|def| def.display_name.clone()),
            ItemId::Material(_) | ItemId::Tool { .. } => Some(id.name().to_string()),
        }
    }

    /// The tool class of `id`, resolved through the registry for pack tools.
    ///
    /// Built-in tools answer from their `(kind, tier)` identity; pack tools
    /// read their `ToolDef`. Non-tools and unknown indices return `None`.
    pub fn tool_kind(&self, id: ItemId) -> Option<ToolKind> {
        match id {
            ItemId::Tool { kind, .. } => Some(kind),
            ItemId::ModTool(index) => self.tool(index).map(|def| def.kind),
            _ => None,
        }
    }

    /// The authored maximum stack size for an insertable `id`, or `None` when
    /// the identity cannot be granted.
    ///
    /// Unknown registry indices, placeholder definitions, and blocks with no
    /// carryable drop row (`stack_limit == 0`) all return `None`. The result is
    /// clamped to `1..=MAX_STACK_COUNT`, so callers can hand it straight to
    /// [`crate::player::inventory::Item::new_with`]. Built-in carryable blocks
    /// answer `MAX_STACK_COUNT` because [`BlockDef::from_builtin`] uses that
    /// row; pack blocks and pack items answer their authored limits; materials
    /// answer [`MATERIAL_STACK_LIMIT`]; every tool answers one.
    pub fn stack_limit(&self, id: ItemId) -> Option<u16> {
        let limit = match id {
            ItemId::Block(block) => self
                .block(block)
                .filter(|def| !def.placeholder)
                .map(|def| def.stack_limit)?,
            ItemId::Item(index) => self
                .item(index)
                .filter(|def| !def.placeholder)
                .map(|def| def.stack_limit)?,
            ItemId::Material(_) => MATERIAL_STACK_LIMIT,
            ItemId::Tool { .. } => 1,
            ItemId::ModTool(index) => {
                self.tool(index)?;
                1
            }
        };
        if limit == 0 {
            return None;
        }
        Some(limit.clamp(1, MAX_STACK_COUNT))
    }

    /// All registered block definitions in raw id order.
    pub fn blocks(&self) -> &[BlockDef] {
        &self.blocks
    }

    /// The definition for `id`, or `None` when the id is unknown.
    ///
    /// Indexing is by raw id and additionally filters on identity, so a forged
    /// [`BlockId::Modded`] payload can never alias a built-in (or another
    /// modded) definition.
    pub fn block(&self, id: BlockId) -> Option<&BlockDef> {
        // A forged `Modded` payload below the built-in count is not a
        // registry index and must never alias a built-in definition.
        if let BlockId::Modded(raw) = id {
            if raw < BUILTIN_BLOCK_COUNT {
                return None;
            }
        }
        self.blocks
            .get(usize::from(id.to_raw()))
            .filter(|def| def.id == id)
    }

    /// Effective seconds to break `block` with `selected`, resolved through the
    /// registry definition's authored hardness and tool class.
    ///
    /// Built-ins answer exactly like [`tools::break_duration`] because
    /// [`BlockDef::from_builtin`] derives both fields from the same accessors.
    /// A pack tool resolves its [`ToolDef`] and authored [`TierDef::speed`]; a
    /// mismatched class keeps the authored hardness and an unknown tool or tier
    /// index falls back to bare-hand timing. Unknown ids and unbreakable
    /// definitions return `None`.
    pub fn break_duration(&self, block: BlockId, selected: Option<Item>) -> Option<f32> {
        let def = self.block(block)?;
        if !matches!(selected.map(|item| item.id()), Some(ItemId::ModTool(_))) {
            return tools::break_duration_for(def.hardness, def.tool_class, selected);
        }
        let Some(ItemId::ModTool(index)) = selected.map(|item| item.id()) else {
            unreachable!("the match above proved a pack tool identity");
        };
        let Some((kind, speed)) = self
            .tool(index)
            .and_then(|tool| self.tier(tool.tier).map(|tier| (tool.kind, tier.speed)))
        else {
            return tools::break_duration_for(def.hardness, def.tool_class, None);
        };
        tools::break_duration_for_pack_tier(def.hardness, def.tool_class, kind, speed)
    }

    /// The insertable drop for `block` and its authored stack limit.
    ///
    /// Returns `None` when the definition is missing, the block authors no
    /// drop, or the drop cannot be resolved to a live identity. Block drops use
    /// the breaking definition's authored limit; item drops use the item
    /// definition's limit; pack tool drops use the non-stackable limit of one;
    /// built-ins use [`ItemId::stack_limit`]. The result is always clamped to
    /// `1..=MAX_STACK_COUNT` so callers can hand it straight to collection.
    pub fn drop_entry(&self, block: BlockId) -> Option<(ItemId, u16)> {
        let def = self.block(block)?;
        let (id, limit) = match def.drop? {
            ItemRef::Block(id) => (ItemId::Block(id), def.stack_limit),
            ItemRef::Item(index) => {
                let item = self.item(index).filter(|def| !def.placeholder)?;
                (ItemId::Item(index), item.stack_limit)
            }
            ItemRef::Builtin(id) => (id, id.stack_limit()),
            ItemRef::Tool(index) => {
                self.tool(index)?;
                (ItemId::ModTool(index), 1)
            }
        };
        Some((id, limit.clamp(1, MAX_STACK_COUNT)))
    }

    /// The definition whose stable short name is exactly `name`.
    ///
    /// This is a linear scan over the frozen table and matches the
    /// unqualified stable name, not the display name.
    pub fn block_by_name(&self, name: &str) -> Option<&BlockDef> {
        self.blocks.iter().find(|def| def.name == name)
    }

    /// All resolved generic item definitions in index order.
    pub fn items(&self) -> &[ItemDef] {
        &self.items
    }

    /// The generic item at dense `index`.
    pub fn item(&self, index: u16) -> Option<&ItemDef> {
        self.items.get(usize::from(index))
    }

    /// The item whose stable id is exactly `name`.
    pub fn item_by_name(&self, name: &str) -> Option<&ItemDef> {
        self.items.iter().find(|def| def.name == name)
    }

    /// All resolved tool tiers: the three built-ins followed by pack tiers.
    pub fn tiers(&self) -> &[TierDef] {
        &self.tiers
    }

    /// The tier at dense `index`.
    pub fn tier(&self, index: u16) -> Option<&TierDef> {
        self.tiers.get(usize::from(index))
    }

    /// The tier whose stable name is exactly `name`.
    pub fn tier_by_name(&self, name: &str) -> Option<&TierDef> {
        self.tiers.iter().find(|def| def.name == name)
    }

    /// All resolved tool definitions in index order.
    pub fn tools(&self) -> &[ToolDef] {
        &self.tools
    }

    /// The tool at dense `index`.
    pub fn tool(&self, index: u16) -> Option<&ToolDef> {
        self.tools.get(usize::from(index))
    }

    /// The tool whose stable id is exactly `name`.
    pub fn tool_by_name(&self, name: &str) -> Option<&ToolDef> {
        self.tools.iter().find(|def| def.name == name)
    }

    /// All resolved recipes: 22 built-ins followed by pack recipes.
    pub fn recipes(&self) -> &[RecipeDef] {
        &self.recipes
    }

    /// The recipe at dense `index`.
    pub fn recipe(&self, index: u16) -> Option<&RecipeDef> {
        self.recipes.get(usize::from(index))
    }

    /// The recipe whose stable name is exactly `name`.
    pub fn recipe_by_name(&self, name: &str) -> Option<&RecipeDef> {
        self.recipes.iter().find(|def| def.name == name)
    }

    /// Number of recipe definitions: the 22 frozen built-ins plus every pack
    /// recipe.
    pub fn recipe_count(&self) -> u16 {
        u16::try_from(self.recipes.len()).expect("recipe table fits the u16 id space")
    }

    /// Enabled, non-placeholder recipes in authored stack order, paired with
    /// their dense index (`< 22` built-in, `>= 22` pack).
    pub fn enabled_recipes(&self) -> impl Iterator<Item = (u16, &RecipeDef)> + '_ {
        self.recipes
            .iter()
            .enumerate()
            .filter(|(_, def)| def.enabled && !def.placeholder)
            .map(|(index, def)| (index as u16, def))
    }

    /// Worldgen rules in authored stack order (later packs last).
    pub fn worldgen_rules(&self) -> &[WorldgenRule] {
        &self.worldgen_rules
    }

    /// Pack-authored block face layers, index-aligned with
    /// `reported layer - PACK_LAYER_BASE`.
    pub fn pack_layers(&self) -> &[Vec<u8>] {
        &self.pack_layers
    }

    /// The pack block layer reported as `layer`, or `None` for built-in and
    /// placeholder layers.
    pub fn pack_layer(&self, layer: u8) -> Option<&[u8]> {
        let index = layer.checked_sub(PACK_LAYER_BASE)?;
        self.pack_layers.get(usize::from(index)).map(Vec::as_slice)
    }

    /// Pack-authored icons, index-aligned with `reported layer - PACK_ICON_BASE`.
    pub fn pack_icons(&self) -> &[Vec<u8>] {
        &self.pack_icons
    }

    /// The pack icon reported as `layer`, or `None` for other layers.
    pub fn pack_icon(&self, layer: u8) -> Option<&[u8]> {
        let index = layer.checked_sub(PACK_ICON_BASE)?;
        self.pack_icons.get(usize::from(index)).map(Vec::as_slice)
    }

    /// Pack identities in the order they were stacked.
    pub fn packs(&self) -> &[PackIdentity] {
        &self.packs
    }

    /// FNV-1a 64 digest of the pack definitions (not built-ins, asset bytes or
    /// diagnostics). Stable across runs for identical input.
    pub fn digest(&self) -> u64 {
        self.digest
    }
}

/// Builds a readable, non-blocking note describing how `current` differs from
/// the registry that wrote a v8 save.
///
/// Pack identities are compared as folder → version maps in sorted-folder
/// order: missing packs, added packs and version changes are reported
/// individually. A digest difference that no pack-level change explains adds
/// `content definitions changed`, so an edited definition is never silently
/// swallowed. `unresolved` is the number of saved names that loaded as
/// deterministic placeholders. Returns `None` iff packs, versions, digest and
/// placeholder count all agree.
pub fn content_mismatch_note(
    saved_packs: &[PackIdentity],
    saved_digest: u64,
    current: &ContentRegistry,
    unresolved: usize,
) -> Option<String> {
    let saved: BTreeMap<&str, Option<&str>> = saved_packs
        .iter()
        .map(|pack| (pack.folder.as_str(), pack.version.as_deref()))
        .collect();
    let loaded: BTreeMap<&str, Option<&str>> = current
        .packs()
        .iter()
        .map(|pack| (pack.folder.as_str(), pack.version.as_deref()))
        .collect();

    let mut notes = Vec::new();
    for folder in saved.keys() {
        if !loaded.contains_key(folder) {
            notes.push(format!("missing pack `{folder}`"));
        }
    }
    for folder in loaded.keys() {
        if !saved.contains_key(folder) {
            notes.push(format!("added pack `{folder}`"));
        }
    }
    for (folder, saved_version) in &saved {
        let Some(loaded_version) = loaded.get(folder) else {
            continue;
        };
        if saved_version != loaded_version {
            notes.push(format!(
                "pack `{folder}` version changed from {} to {}",
                saved_version.unwrap_or("unspecified"),
                loaded_version.unwrap_or("unspecified"),
            ));
        }
    }

    // A digest difference with identical pack identities means a definition
    // (or an authored asset key) changed underneath the same stack.
    if saved_digest != current.digest() && notes.is_empty() {
        notes.push("content definitions changed".to_string());
    }

    if unresolved > 0 {
        notes.push(format!(
            "{unresolved} saved content name{} {} unavailable and render as placeholders",
            if unresolved == 1 { "" } else { "s" },
            if unresolved == 1 { "is" } else { "are" },
        ));
    }

    if notes.is_empty() {
        None
    } else {
        Some(notes.join("; "))
    }
}

fn builtin_blocks() -> Vec<BlockDef> {
    (0..BUILTIN_BLOCK_COUNT)
        .map(|raw| {
            BlockDef::from_builtin(BlockId::from_raw(raw))
                .expect("every built-in raw id has a definition")
        })
        .collect()
}

fn builtin_tiers() -> Vec<TierDef> {
    [ToolTier::Wood, ToolTier::Stone, ToolTier::Iron]
        .into_iter()
        .enumerate()
        .map(|(index, tier)| TierDef {
            index: index as u16,
            name: names::builtin_tier_id_name(tier).to_string(),
            display_name: tier.name().to_string(),
            speed: f32::from(tools::tier_speed_multiplier(tier)),
            durability: tier.max_durability(),
            damage: tools::tier_sword_damage(tier),
        })
        .collect()
}

fn builtin_recipes() -> Vec<RecipeDef> {
    RECIPES
        .iter()
        .enumerate()
        .map(|(index, recipe)| builtin_recipe_def(index as u16, recipe))
        .collect()
}

fn builtin_recipe_def(index: u16, recipe: &Recipe) -> RecipeDef {
    let mut key = BTreeMap::new();
    for row in recipe.pattern {
        for byte in row.bytes() {
            if key.contains_key(&char::from(byte)) {
                continue;
            }
            if let Some(item) = inventory::pattern_item(byte) {
                key.insert(char::from(byte), ItemRef::from_item_id(item));
            }
        }
    }
    RecipeDef {
        index,
        name: names::builtin_recipe_id_name(recipe.id)
            .expect("every built-in recipe has a frozen name")
            .to_string(),
        pattern: recipe
            .pattern
            .iter()
            .map(|row| (*row).to_string())
            .collect(),
        key,
        output: ItemRef::from_item_id(recipe.output),
        count: recipe.output_count,
        enabled: true,
        placeholder: false,
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
            let expected_drop =
                if id.break_duration_secs().is_some() && id.mined_drop().is_carryable() {
                    Some(ItemRef::Block(id.mined_drop()))
                } else {
                    None
                };
            assert_eq!(def.drop, expected_drop, "drop mismatch for {id:?}");
            assert_eq!(def.tool_class, tools::block_tool_class(id));
            let expected_stack = if expected_drop.is_some() {
                MAX_STACK_COUNT
            } else {
                0
            };
            assert_eq!(def.stack_limit, expected_stack);
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

    #[test]
    fn builtin_registry_has_three_tiers_and_22_recipes() {
        let registry = ContentRegistry::builtin();
        assert_eq!(registry.tiers().len(), 3);
        assert_eq!(
            registry
                .tiers()
                .iter()
                .map(|tier| tier.name.as_str())
                .collect::<Vec<_>>(),
            vec!["wood", "stone", "iron"]
        );
        assert_eq!(
            registry
                .tiers()
                .iter()
                .map(|tier| tier.display_name.as_str())
                .collect::<Vec<_>>(),
            vec!["Wood", "Stone", "Iron"]
        );
        for (index, tier) in registry.tiers().iter().enumerate() {
            assert_eq!(usize::from(tier.index), index);
        }
        assert_eq!(registry.tiers()[0].speed, 2.0);
        assert_eq!(registry.tiers()[1].speed, 4.0);
        assert_eq!(registry.tiers()[2].speed, 6.0);
        assert_eq!(
            registry.tiers()[0].durability,
            ToolTier::Wood.max_durability()
        );
        assert_eq!(registry.tiers()[0].damage, 4);
        assert_eq!(registry.tiers()[1].damage, 5);
        assert_eq!(registry.tiers()[2].damage, 6);

        assert_eq!(registry.recipes().len(), 22);
        for (index, recipe) in registry.recipes().iter().enumerate() {
            assert_eq!(usize::from(recipe.index), index);
            assert!(recipe.enabled);
            assert!(!recipe.placeholder);
            assert!(!recipe.pattern.is_empty());
            assert!(recipe.key.values().all(|reference| match reference {
                ItemRef::Block(_) | ItemRef::Builtin(_) => true,
                ItemRef::Item(_) | ItemRef::Tool(_) => false,
            }));
        }
        assert_eq!(
            registry.recipe(0).map(|recipe| recipe.name.as_str()),
            Some("planks")
        );
        assert_eq!(
            registry
                .recipe_by_name("planks")
                .map(|recipe| recipe.output),
            Some(ItemRef::Block(BlockId::OakPlanks))
        );
        assert!(registry.items().is_empty());
        assert!(registry.tools().is_empty());
        assert!(registry.worldgen_rules().is_empty());
        assert!(registry.pack_layers().is_empty());
        assert!(registry.pack_icons().is_empty());
        assert!(registry.packs().is_empty());
        assert_eq!(registry.digest(), EMPTY_DIGEST);
    }

    #[test]
    fn placeholder_appends_are_deterministic_and_leave_the_builtin_registry_alone() {
        let builtin_blocks = ContentRegistry::builtin().blocks().len();
        let builtin_digest = ContentRegistry::builtin().digest();
        let mut registry = ContentRegistry::builtin().clone();

        let ruby_ore = registry.append_placeholder_block("mymod:ruby_ore");
        let other_ore = registry.append_placeholder_block("mymod:other_ore");
        let expected_first = BlockId::Modded(
            u16::try_from(builtin_blocks).expect("test table fits the u16 id space"),
        );
        assert_eq!(ruby_ore, expected_first);
        assert_eq!(
            other_ore,
            BlockId::Modded(u16::try_from(builtin_blocks + 1).expect("fits")),
        );

        let def = registry
            .block(ruby_ore)
            .expect("appended definition resolves");
        assert!(def.placeholder);
        assert_eq!(def.name, "mymod:ruby_ore");
        assert_eq!(def.display_name, "mymod:ruby_ore");
        assert_eq!(def.faces, [PLACEHOLDER_BLOCK_LAYER; 6]);
        assert_eq!(def.hardness, None);
        assert_eq!(def.drop, None);
        assert_eq!(def.tool_class, None);
        assert_eq!(def.stack_limit, MAX_STACK_COUNT);
        assert_eq!(registry.block_name(ruby_ore), Some("mymod:ruby_ore"));
        assert_eq!(registry.block_name(other_ore), Some("mymod:other_ore"));

        // Placeholder appends are not pack definitions: identity and pack
        // stack must stay exactly as they were.
        assert_eq!(registry.digest(), builtin_digest);
        assert!(registry.packs().is_empty());

        // The shared built-in table was cloned, not mutated.
        assert_eq!(ContentRegistry::builtin().blocks().len(), builtin_blocks);
        assert!(ContentRegistry::builtin().block(ruby_ore).is_none());
        assert_eq!(ContentRegistry::builtin().digest(), builtin_digest);
    }

    #[test]
    fn placeholder_items_are_deterministic_and_nameable() {
        let mut registry = ContentRegistry::builtin().clone();
        assert!(registry.items().is_empty());

        let ruby = registry.append_placeholder_item("mymod:ruby");
        let shard = registry.append_placeholder_item("mymod:ruby_shard");
        assert_eq!(ruby, 0);
        assert_eq!(shard, 1);

        let def = registry.item(ruby).expect("appended item resolves");
        assert!(def.placeholder);
        assert_eq!(def.index, ruby);
        assert_eq!(def.name, "mymod:ruby");
        assert_eq!(def.display_name, "mymod:ruby");
        assert_eq!(def.icon_layer, PLACEHOLDER_ICON_LAYER);
        assert_eq!(def.stack_limit, MAX_STACK_COUNT);
        assert_eq!(def.food, None);
        assert_eq!(registry.item_def_name(ruby), Some("mymod:ruby"));
        assert_eq!(registry.item_def_name(shard), Some("mymod:ruby_shard"));
        assert_eq!(registry.item_def_name(2), None);
        assert!(registry.item_by_name("mymod:ruby").is_some());

        assert!(ContentRegistry::builtin().items().is_empty());
        assert_eq!(registry.digest(), ContentRegistry::builtin().digest());
        assert!(registry.packs().is_empty());
    }

    #[test]
    fn builtin_arc_is_shared_and_matches_builtin() {
        let first = ContentRegistry::builtin_arc();
        let second = ContentRegistry::builtin_arc();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(
            first.blocks().len(),
            ContentRegistry::builtin().blocks().len()
        );
        assert_eq!(first.digest(), ContentRegistry::builtin().digest());
        assert_eq!(
            first.packs().len(),
            ContentRegistry::builtin().packs().len()
        );
    }

    fn pack(
        folder: &str,
        version: Option<&str>,
        spec: PackContentSpec,
    ) -> (PackIdentity, PackContentSpec) {
        (
            PackIdentity {
                folder: folder.to_string(),
                version: version.map(str::to_string),
            },
            spec,
        )
    }

    /// A stack entry that changes the digest without introducing new ids: it
    /// patches a built-in display name, so no assets are needed.
    fn stone_display(display: &str) -> PackContentSpec {
        let mut spec = PackContentSpec::default();
        spec.block_patches.push(crate::content::spec::BlockPatch {
            id: "stone".to_string(),
            display_name: Some(display.to_string()),
            hardness: None,
            drop: None,
            tool: None,
            stack_limit: None,
        });
        spec
    }

    #[test]
    fn content_mismatch_note_is_none_for_matching_content() {
        assert_eq!(
            content_mismatch_note(&[], EMPTY_DIGEST, ContentRegistry::builtin(), 0),
            None
        );

        let (registry, report) = build_registry(
            ContentRegistry::builtin(),
            &[pack("alpha", Some("1.0"), stone_display("Stone"))],
        );
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        assert_eq!(
            content_mismatch_note(registry.packs(), registry.digest(), &registry, 0),
            None
        );
    }

    #[test]
    fn content_mismatch_note_reports_missing_packs_and_placeholders() {
        let (saved, _) = build_registry(
            ContentRegistry::builtin(),
            &[
                pack("alpha", Some("1.0"), stone_display("Stone")),
                pack("beta", Some("2.0"), stone_display("Rock")),
            ],
        );
        let (current, _) = build_registry(
            ContentRegistry::builtin(),
            &[pack("alpha", Some("1.0"), stone_display("Stone"))],
        );

        let note = content_mismatch_note(saved.packs(), saved.digest(), &current, 2)
            .expect("a missing pack is a mismatch");
        assert!(note.contains("missing pack `beta`"), "{note}");
        assert!(
            note.contains("2 saved content names are unavailable and render as placeholders"),
            "{note}"
        );
        assert!(
            !note.contains("content definitions changed"),
            "a pack-level difference explains the digest: {note}"
        );
    }

    #[test]
    fn content_mismatch_note_reports_added_packs_and_version_changes() {
        let saved = build_registry(
            ContentRegistry::builtin(),
            &[pack("alpha", Some("1.0"), stone_display("Stone"))],
        )
        .0;
        let added = build_registry(
            ContentRegistry::builtin(),
            &[
                pack("alpha", Some("1.0"), stone_display("Stone")),
                pack("beta", None, PackContentSpec::default()),
            ],
        )
        .0;
        let note = content_mismatch_note(saved.packs(), saved.digest(), &added, 0)
            .expect("an added pack is a mismatch");
        assert_eq!(note, "added pack `beta`");

        let changed = build_registry(
            ContentRegistry::builtin(),
            &[pack("alpha", Some("2.0"), stone_display("Stone"))],
        )
        .0;
        let note = content_mismatch_note(saved.packs(), saved.digest(), &changed, 0)
            .expect("a version change is a mismatch");
        assert_eq!(note, "pack `alpha` version changed from 1.0 to 2.0");
    }

    #[test]
    fn content_mismatch_note_reports_an_unexplained_digest_change() {
        let saved = build_registry(
            ContentRegistry::builtin(),
            &[pack("alpha", Some("1.0"), stone_display("Stone"))],
        )
        .0;
        let changed = build_registry(
            ContentRegistry::builtin(),
            &[pack("alpha", Some("1.0"), stone_display("Rock"))],
        )
        .0;
        assert_ne!(saved.digest(), changed.digest());
        assert_eq!(
            content_mismatch_note(saved.packs(), saved.digest(), &changed, 0),
            Some("content definitions changed".to_string())
        );
    }

    #[test]
    fn item_ref_maps_only_insertable_identities() {
        use crate::player::inventory::Material;

        assert_eq!(
            ItemRef::Block(BlockId::Stone).item_id(),
            Some(ItemId::Block(BlockId::Stone))
        );
        assert_eq!(
            ItemRef::Builtin(ItemId::Material(Material::Stick)).item_id(),
            Some(ItemId::Material(Material::Stick))
        );
        assert_eq!(ItemRef::Item(7).item_id(), Some(ItemId::Item(7)));
        assert_eq!(
            ItemRef::Tool(3).item_id(),
            Some(ItemId::ModTool(3)),
            "pack tools map to the widened modded tool identity"
        );
        assert_eq!(ItemRef::Tool(0).item_id(), Some(ItemId::ModTool(0)));
    }

    #[test]
    fn registry_names_and_kinds_resolve_pack_tools() {
        use crate::content::spec::{PackAsset, TierEntry, ToolEntry};
        use crate::player::inventory::Material;

        let mut spec = PackContentSpec {
            tiers: vec![TierEntry {
                id: "mymod:ruby".to_string(),
                display_name: Some("Ruby".to_string()),
                speed: 9.0,
                durability: 900,
                damage: 9,
            }],
            tools: vec![ToolEntry {
                id: "mymod:ruby_pickaxe".to_string(),
                display_name: Some("Ruby Pickaxe".to_string()),
                kind: ToolKind::Pickaxe,
                tier: "mymod:ruby".to_string(),
                icon: "ruby_pickaxe.png".to_string(),
            }],
            ..PackContentSpec::default()
        };
        spec.assets.insert(
            "ruby_pickaxe.png".to_string(),
            PackAsset {
                rgba: vec![0u8; 16 * 16 * 4],
                source_side: 16,
            },
        );
        let (registry, report) =
            build_registry(ContentRegistry::builtin(), &[pack("fixture", None, spec)]);
        assert_eq!(report.errors(), 0);

        assert_eq!(registry.tool_def_name(0), Some("mymod:ruby_pickaxe"));
        assert_eq!(registry.tool_def_name(99), None);
        assert_eq!(
            registry.item_display_name(ItemId::ModTool(0)),
            Some("Ruby Pickaxe".to_string())
        );
        assert_eq!(
            registry.tool_kind(ItemId::ModTool(0)),
            Some(ToolKind::Pickaxe)
        );
        assert_eq!(registry.item_display_name(ItemId::ModTool(99)), None);
        assert_eq!(registry.tool_kind(ItemId::ModTool(99)), None);
        assert_eq!(registry.tool_kind(ItemId::Material(Material::Stick)), None);

        // Built-ins keep their static labels; blocks read the registry def.
        assert_eq!(
            registry.item_display_name(ItemId::Material(Material::Stick)),
            Some("Stick".to_string())
        );
        assert_eq!(
            registry.item_display_name(ItemId::Tool {
                kind: ToolKind::Pickaxe,
                tier: ToolTier::Iron
            }),
            Some("Iron Pickaxe".to_string())
        );
        assert_eq!(
            registry.item_display_name(ItemId::Block(BlockId::Stone)),
            Some("Stone".to_string())
        );
        assert_eq!(
            registry.tool_kind(ItemId::Tool {
                kind: ToolKind::Axe,
                tier: ToolTier::Wood
            }),
            Some(ToolKind::Axe)
        );
    }

    /// Clones the built-in registry and appends definitions whose drops cover
    /// every [`ItemRef`] arm, so `drop_entry` is exercised without pack IO.
    fn registry_with_drops() -> (ContentRegistry, [BlockId; 4]) {
        let mut registry = ContentRegistry::builtin().clone();
        let item_drop = BlockId::Modded(BUILTIN_BLOCK_COUNT);
        let block_drop = BlockId::Modded(BUILTIN_BLOCK_COUNT + 1);
        let tool_drop = BlockId::Modded(BUILTIN_BLOCK_COUNT + 2);
        let missing_item_drop = BlockId::Modded(BUILTIN_BLOCK_COUNT + 3);

        registry.items.push(ItemDef {
            index: 0,
            name: "mymod:ruby".to_string(),
            display_name: "Ruby".to_string(),
            icon_layer: PLACEHOLDER_ICON_LAYER,
            stack_limit: 12,
            food: None,
            placeholder: false,
            placeholder_tool: false,
        });
        registry.tiers.push(TierDef {
            index: 3,
            name: "mymod:ruby".to_string(),
            display_name: "Ruby".to_string(),
            speed: 9.0,
            durability: 900,
            damage: 9,
        });
        registry.tools.push(ToolDef {
            index: 0,
            name: "mymod:ruby_pickaxe".to_string(),
            display_name: "Ruby Pickaxe".to_string(),
            kind: ToolKind::Pickaxe,
            tier: 3,
            icon_layer: PLACEHOLDER_ICON_LAYER,
        });

        let mut push_block = |id: BlockId, drop: ItemRef, stack_limit: u16| {
            registry.blocks.push(BlockDef {
                id,
                name: format!("test:{id:?}"),
                display_name: format!("Test {id:?}"),
                faces: [PLACEHOLDER_BLOCK_LAYER; 6],
                hardness: Some(1.0),
                drop: Some(drop),
                tool_class: None,
                stack_limit,
                placeholder: false,
            });
        };
        push_block(item_drop, ItemRef::Item(0), 64);
        push_block(block_drop, ItemRef::Block(item_drop), 7);
        push_block(tool_drop, ItemRef::Tool(0), 64);
        push_block(missing_item_drop, ItemRef::Item(9), 64);
        (
            registry,
            [item_drop, block_drop, tool_drop, missing_item_drop],
        )
    }

    #[test]
    fn drop_entry_resolves_authored_identities_and_limits() {
        let (registry, [item_drop, block_drop, tool_drop, missing_item_drop]) =
            registry_with_drops();

        // Built-ins keep the historical drop and full block limit.
        assert_eq!(
            registry.drop_entry(BlockId::Stone),
            Some((ItemId::Block(BlockId::Cobblestone), MAX_STACK_COUNT))
        );
        assert_eq!(registry.drop_entry(BlockId::Air), None);
        assert_eq!(registry.drop_entry(BlockId::Bedrock), None);
        assert_eq!(
            registry.drop_entry(BlockId::Modded(BUILTIN_BLOCK_COUNT + 99)),
            None,
            "unknown ids have no drop"
        );

        // Item drops use the target item's authored limit; block drops use the
        // breaking definition's authored limit; tools are non-stackable.
        assert_eq!(registry.drop_entry(item_drop), Some((ItemId::Item(0), 12)));
        assert_eq!(
            registry.drop_entry(block_drop),
            Some((ItemId::Block(item_drop), 7))
        );
        assert_eq!(
            registry.drop_entry(tool_drop),
            Some((ItemId::ModTool(0), 1)),
            "an authored pack tool drop resolves to the widened identity"
        );

        // Unresolved item and tool indices cannot be inserted.
        assert_eq!(registry.drop_entry(missing_item_drop), None);
        let mut unknown_tool = ContentRegistry::builtin().clone();
        let unknown_tool_block = BlockId::Modded(BUILTIN_BLOCK_COUNT);
        unknown_tool.blocks.push(BlockDef {
            id: unknown_tool_block,
            name: "test:unknown_tool".to_string(),
            display_name: "Test Unknown Tool".to_string(),
            faces: [PLACEHOLDER_BLOCK_LAYER; 6],
            hardness: Some(1.0),
            drop: Some(ItemRef::Tool(9)),
            tool_class: None,
            stack_limit: 64,
            placeholder: false,
        });
        assert_eq!(unknown_tool.drop_entry(unknown_tool_block), None);

        // A placeholder item identity never becomes a live drop.
        let mut placeholder = ContentRegistry::builtin().clone();
        let placeholder_index = placeholder.append_placeholder_item("save:missing_gem");
        let placeholder_block = placeholder.append_placeholder_block("save:placeholder_source");
        placeholder.blocks[usize::from(placeholder_block.to_raw())].drop =
            Some(ItemRef::Item(placeholder_index));
        assert_eq!(placeholder.drop_entry(placeholder_block), None);
    }

    #[test]
    fn drop_entry_clamps_authored_limits_into_the_insertable_range() {
        let (mut registry, [item_drop, block_drop, ..]) = registry_with_drops();
        let block_index = usize::from(block_drop.to_raw());

        registry.items[0].stack_limit = 0;
        assert_eq!(
            registry.drop_entry(item_drop),
            Some((ItemId::Item(0), 1)),
            "a zero authored item limit clamps to one"
        );

        registry.items[0].stack_limit = u16::MAX;
        assert_eq!(
            registry.drop_entry(item_drop),
            Some((ItemId::Item(0), MAX_STACK_COUNT)),
            "an oversized authored item limit clamps to the block cap"
        );

        registry.blocks[block_index].stack_limit = 0;
        assert_eq!(
            registry.drop_entry(block_drop),
            Some((ItemId::Block(item_drop), 1)),
            "a zero authored block limit clamps to one"
        );
        registry.blocks[block_index].stack_limit = u16::MAX;
        assert_eq!(
            registry.drop_entry(block_drop),
            Some((ItemId::Block(item_drop), MAX_STACK_COUNT))
        );
    }

    #[test]
    fn break_duration_matches_builtin_and_resolves_pack_hardness() {
        let registry = ContentRegistry::builtin();
        for block in [
            BlockId::Stone,
            BlockId::OakLog,
            BlockId::Air,
            BlockId::Bedrock,
        ] {
            assert_eq!(
                registry.break_duration(block, None),
                tools::break_duration(block, None),
                "bare-hand answer diverged for {block:?}"
            );
            for item in [
                Item::new(
                    ItemId::Tool {
                        kind: ToolKind::Pickaxe,
                        tier: ToolTier::Iron,
                    },
                    1,
                )
                .expect("built-in pickaxe"),
                Item::new(ItemId::Block(BlockId::OakPlanks), 1).expect("block item"),
            ] {
                assert_eq!(
                    registry.break_duration(block, Some(item)),
                    tools::break_duration(block, Some(item)),
                    "built-in selection diverged for {block:?}"
                );
            }
        }

        let (mut registry, [ruby, ..]) = registry_with_drops();
        registry.blocks[usize::from(ruby.to_raw())].hardness = Some(0.5);
        assert_eq!(registry.break_duration(ruby, None), Some(0.5));
        registry.blocks[usize::from(ruby.to_raw())].hardness = None;
        assert_eq!(registry.break_duration(ruby, None), None);
    }

    #[test]
    fn pack_tier_speed_shortens_a_matched_block_and_unknown_indices_fall_back() {
        let (mut registry, [ruby, ..]) = registry_with_drops();
        let ruby_block = usize::from(ruby.to_raw());
        registry.blocks[ruby_block].hardness = Some(3.0);
        registry.blocks[ruby_block].tool_class = Some(ToolKind::Pickaxe);

        let ruby_pickaxe = Item::new_with(&registry, ItemId::ModTool(0), 1).expect("pack pickaxe");
        let iron_pickaxe = Item::new(
            ItemId::Tool {
                kind: ToolKind::Pickaxe,
                tier: ToolTier::Iron,
            },
            1,
        )
        .expect("built-in pickaxe");

        let ruby_duration = registry
            .break_duration(ruby, Some(ruby_pickaxe))
            .expect("the pack block is breakable");
        let iron_duration = registry
            .break_duration(ruby, Some(iron_pickaxe))
            .expect("the pack block is breakable");
        assert!((ruby_duration - 3.0 / 9.0).abs() < 1e-6);
        assert!((iron_duration - 3.0 / 6.0).abs() < 1e-6);
        assert!(
            ruby_duration < iron_duration,
            "the authored 9x tier must beat the 6x iron tier"
        );

        // A mismatched pack class keeps the authored hardness.
        let mut mismatched = registry.clone();
        mismatched.tools[0].kind = ToolKind::Sword;
        let sword = Item::new_with(&mismatched, ItemId::ModTool(0), 1).expect("pack sword");
        assert_eq!(mismatched.break_duration(ruby, Some(sword)), Some(3.0));

        // An item whose tool index does not exist in the resolving registry
        // falls back to bare-hand timing instead of aliasing another identity.
        let foreign = Item::new_with(&registry, ItemId::ModTool(0), 1).expect("pack pickaxe");
        let mut no_tools = registry.clone();
        no_tools.tools.clear();
        assert_eq!(no_tools.break_duration(ruby, Some(foreign)), Some(3.0));
    }

    #[test]
    fn placeholder_or_unknown_recipe_key_never_matches() {
        use crate::player::inventory::{
            CraftError, CraftingGrid, GridKind, Inventory, Item, ItemId, Material,
        };

        let mut registry = ContentRegistry::builtin().clone();
        let placeholder = registry.append_placeholder_item("save:missing_gem");

        let placeholder_index = registry.recipe_count();
        registry.recipes.push(RecipeDef {
            index: placeholder_index,
            name: "save:placeholder_recipe".to_string(),
            pattern: vec!["Q".to_string()],
            key: std::collections::BTreeMap::from([('Q', ItemRef::Item(placeholder))]),
            output: ItemRef::Block(BlockId::Stone),
            count: 1,
            enabled: true,
            placeholder: false,
        });
        let unknown_index = registry.recipe_count();
        registry.recipes.push(RecipeDef {
            index: unknown_index,
            name: "save:unknown_recipe".to_string(),
            pattern: vec!["U".to_string()],
            key: std::collections::BTreeMap::from([('U', ItemRef::Item(u16::MAX))]),
            output: ItemRef::Block(BlockId::Stone),
            count: 1,
            enabled: true,
            placeholder: false,
        });

        // A grid holding the placeholder identity itself must not match the
        // recipe keyed to that placeholder.
        let mut grid = CraftingGrid::new(GridKind::Player);
        grid.set(
            0,
            Some(
                Item::new_with(&registry, ItemId::Item(placeholder), 1)
                    .expect("placeholder item constructs"),
            ),
        )
        .expect("player cell");
        assert_eq!(grid.preview_with(&registry), None);
        let before = grid.clone();
        assert_eq!(
            grid.take_result_with(&registry, &mut Inventory::empty()),
            Err(CraftError::NoRecipe)
        );
        assert_eq!(grid, before, "a failed match consumes nothing");

        // A key pointing at an absent registry index also fails closed.
        let mut unknown_grid = CraftingGrid::new(GridKind::Player);
        unknown_grid
            .set(
                0,
                Some(Item::new(ItemId::Material(Material::Stick), 1).expect("built-in stick")),
            )
            .expect("player cell");
        assert_eq!(unknown_grid.preview_with(&registry), None);
    }

    #[test]
    fn content_mismatch_note_uses_singular_and_plural_placeholder_counts() {
        assert_eq!(
            content_mismatch_note(&[], EMPTY_DIGEST, ContentRegistry::builtin(), 1),
            Some("1 saved content name is unavailable and render as placeholders".to_string())
        );
        assert_eq!(
            content_mismatch_note(&[], EMPTY_DIGEST, ContentRegistry::builtin(), 3),
            Some("3 saved content names are unavailable and render as placeholders".to_string())
        );
    }

    // -- Palette stack limits (S07-T02) ------------------------------------

    /// The checked-in reference pack through the same production mapping as
    /// `main.rs::build_session_registry`.
    fn reference_registry() -> ContentRegistry {
        let spec = crate::pack::spec::load_spec_from_dir(std::path::Path::new(
            "tests/fixtures/packs/mymod-ruby",
        ));
        let pack = (
            PackIdentity {
                folder: spec.folder.clone(),
                version: spec.version.clone(),
            },
            spec.content.clone(),
        );
        let (registry, report) = build_registry(ContentRegistry::builtin(), &[pack]);
        assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
        registry
    }

    #[test]
    fn palette_stack_limit_matches_builtin_and_pack_authored_limits() {
        use crate::player::inventory::Material;

        let registry = reference_registry();

        assert_eq!(
            registry.stack_limit(ItemId::Block(BlockId::Stone)),
            Some(MAX_STACK_COUNT),
            "built-in carryable blocks answer the authored 999 row"
        );
        assert_eq!(
            registry.stack_limit(ItemId::Material(Material::Stick)),
            Some(MATERIAL_STACK_LIMIT)
        );
        assert_eq!(
            registry.stack_limit(ItemId::Tool {
                kind: ToolKind::Pickaxe,
                tier: ToolTier::Iron,
            }),
            Some(1)
        );

        let stew = registry
            .item_by_name("mymod:ruby_stew")
            .expect("registered pack stew")
            .index;
        assert_eq!(registry.stack_limit(ItemId::Item(stew)), Some(16));
        let ruby = registry
            .item_by_name("mymod:ruby")
            .expect("registered pack gem")
            .index;
        assert_eq!(registry.stack_limit(ItemId::Item(ruby)), Some(64));

        let ore = registry
            .block_by_name("mymod:ruby_ore")
            .expect("registered pack ore")
            .id;
        assert_eq!(registry.stack_limit(ItemId::Block(ore)), Some(64));

        let pickaxe = registry
            .tool_by_name("mymod:ruby_pickaxe")
            .expect("registered pack pickaxe")
            .index;
        assert_eq!(registry.stack_limit(ItemId::ModTool(pickaxe)), Some(1));
    }

    #[test]
    fn palette_stack_limit_rejects_placeholders_and_unknown_indices() {
        use crate::player::inventory::Material;

        let mut registry = reference_registry();
        let placeholder_block = registry.append_placeholder_block("save:missing_block");
        let placeholder_item = registry.append_placeholder_item("save:missing_item");

        assert_eq!(
            registry.stack_limit(ItemId::Block(placeholder_block)),
            None,
            "placeholder art is never grantable"
        );
        assert_eq!(
            registry.stack_limit(ItemId::Item(placeholder_item)),
            None,
            "placeholder art is never grantable"
        );
        assert_eq!(registry.stack_limit(ItemId::Item(u16::MAX)), None);
        assert_eq!(registry.stack_limit(ItemId::ModTool(u16::MAX)), None);
        assert_eq!(
            registry.stack_limit(ItemId::Block(BlockId::Bedrock)),
            None,
            "a block with no carryable drop row cannot be granted"
        );
        assert_eq!(
            registry.stack_limit(ItemId::Material(Material::Stick)),
            Some(MATERIAL_STACK_LIMIT),
            "unknown registry indices must not affect static identities"
        );
    }
}
