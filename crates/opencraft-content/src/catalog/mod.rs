//! Neutral content catalog: definition rows, placeholder policy and the
//! changed-stack notice.
//!
//! The crate's pack parser produces an authored [`crate::spec::PackContentSpec`];
//! this module owns the neutral, raw-`u16` catalog that a deterministic
//! pack-stack fold produces from it. Nothing here names a game block id,
//! session state, GPU layer or worldgen consumer: identities are raw `u16`
//! ids into the catalog tables, built-in item identities are frozen index
//! tags, and biomes are the neutral [`BiomeTag`] vocabulary.
//!
//! The catalog mirrors the game registry's tables one-for-one, so a game-side
//! adapter is a field-by-field conversion rather than a second source of
//! truth. The FNV-1a content digest lives in [`digest`], and the reserved
//! layer/icon constants here are the single authoritative values both the
//! crate and the game report.

mod build;
mod digest;

pub use build::build_catalog;

use std::collections::BTreeMap;

use crate::spec::{ContentDiagnostic, FoodSpec, Severity, ToolKind};

/// First texture-array layer reserved for pack-authored block faces.
pub const PACK_LAYER_BASE: u8 = 48;

/// Exclusive upper bound for pack-authored block face layers (160 pack layers).
pub const PACK_LAYER_CAP: u8 = 208;

/// First icon layer reserved for pack-authored item/tool icons.
pub const PACK_ICON_BASE: u8 = 32;

/// Exclusive upper bound for pack-authored icon layers (192 pack icons).
pub const PACK_ICON_CAP: u8 = 224;

/// Reserved block face layer used when a pack asset is missing or the layer
/// space overflows: the built-in checker placeholder.
///
/// Starts at 26 so a fallback never aliases layer 25, the built-in
/// socket-marker art that packs cannot override.
pub const PLACEHOLDER_BLOCK_LAYER: u8 = 26;

/// Reserved icon layer used when a pack icon is missing or icon space
/// overflows.
pub const PLACEHOLDER_ICON_LAYER: u8 = 27;

/// Digest of a catalog with no packs: the FNV-1a 64 offset basis.
///
/// The digest excludes built-in definitions, asset bytes and diagnostics, so
/// a catalog built from an empty stack is exactly this value.
pub const EMPTY_DIGEST: u64 = 0xcbf2_9ce4_8422_2325;

/// Maximum number of worldgen rules the stack may contribute.
pub const WORLDGEN_RULE_CAP: usize = 8;

/// Identity of one pack in the stack, hashed into the content digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackIdentity {
    /// Pack folder name (stable identity inside `packs/`).
    pub folder: String,
    /// Optional authored pack version.
    pub version: Option<String>,
}

/// Outcome of one deterministic catalog build.
///
/// `entries` is the number of authored content entries considered across the
/// stack (blocks, items, tiers, tools, recipes, worldgen rules and patches).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContentLoadReport {
    /// Authored content entries considered.
    pub entries: usize,
    /// Per-entry problems collected while building the catalog.
    pub diagnostics: Vec<ContentDiagnostic>,
}

impl ContentLoadReport {
    /// Number of error-severity diagnostics.
    pub fn errors(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
            .count()
    }

    /// Number of warning-severity diagnostics.
    pub fn warnings(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Warning)
            .count()
    }
}

/// Provenance of one frozen pack-authored block layer or icon.
///
/// `path` is the pack-relative authored path exactly as the pack names it. A
/// source is metadata only: it identifies which `(pack, path)` produced an
/// allocation slot. Sources never enter the content digest.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LayerSource {
    /// Pack folder name (stable identity inside `packs/`).
    pub folder: String,
    /// Pack-relative authored path.
    pub path: String,
}

/// Neutral biome tag for declarative worldgen patches.
///
/// Declaration order matches [`crate::vocabulary::BIOME_NAMES`] and is frozen:
/// the [`Self::index`] tag is part of the content digest.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum BiomeTag {
    Plains,
    Forest,
    Mountains,
    Ocean,
}

impl BiomeTag {
    /// Digest tag for this biome (`plains`, `forest`, `mountains`, `ocean`).
    pub const fn index(self) -> u8 {
        match self {
            BiomeTag::Plains => 0,
            BiomeTag::Forest => 1,
            BiomeTag::Mountains => 2,
            BiomeTag::Ocean => 3,
        }
    }

    /// The tag whose canonical vocabulary name is exactly `name`.
    pub fn from_name(name: &str) -> Option<BiomeTag> {
        crate::vocabulary::BIOME_NAMES
            .iter()
            .position(|known| *known == name)
            .and_then(|index| u8::try_from(index).ok())
            .and_then(BiomeTag::from_index)
    }

    /// The tag for a frozen digest index, or `None` when it is out of range.
    pub const fn from_index(index: u8) -> Option<BiomeTag> {
        match index {
            0 => Some(BiomeTag::Plains),
            1 => Some(BiomeTag::Forest),
            2 => Some(BiomeTag::Mountains),
            3 => Some(BiomeTag::Ocean),
            _ => None,
        }
    }
}

/// A reference to an item identity, raw-typed for neutrality.
///
/// Built-in identities keep their frozen vocabulary index so the content
/// digest hashes exactly the same bytes for a built-in material or tool
/// whether the caller is the crate or the game adapter.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ItemRef {
    /// A catalog block identity (raw id).
    Block(u16),
    /// A built-in non-block item; index into
    /// [`crate::vocabulary::MATERIAL_ID_NAMES`].
    BuiltinMaterial(u8),
    /// A built-in non-block tool; `tier` is an index into
    /// [`crate::vocabulary::TIER_ID_NAMES`] (wood, stone, iron).
    BuiltinTool { kind: ToolKind, tier: u8 },
    /// A catalog-allocated pack item index.
    Item(u16),
    /// A catalog-allocated pack tool index.
    Tool(u16),
}

/// A resolved generic item definition.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemDef {
    /// Dense catalog index (`ItemRef::Item`).
    pub index: u16,
    /// Namespaced stable id (for example `mymod:ruby`).
    pub name: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Reported icon layer; index into [`Catalog::pack_icons`] as
    /// `icon_layer - PACK_ICON_BASE`.
    pub icon_layer: u8,
    /// Maximum stack size.
    pub stack_limit: u16,
    /// Food behaviour, when the item is edible.
    pub food: Option<FoodSpec>,
    /// Whether this definition is placeholder art awaiting pack content.
    pub placeholder: bool,
    /// Whether this placeholder stands in for a pack tool whose pack is
    /// missing.
    pub placeholder_tool: bool,
}

/// A resolved tool tier definition.
#[derive(Clone, Debug, PartialEq)]
pub struct TierDef {
    /// Dense catalog index; built-ins occupy `0..=2`.
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
    /// Dense catalog index (`ItemRef::Tool`).
    pub index: u16,
    /// Namespaced stable id (for example `mymod:ruby_pickaxe`).
    pub name: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Tool behaviour class.
    pub kind: ToolKind,
    /// Index into [`Catalog::tiers`].
    pub tier: u16,
    /// Reported icon layer; index into [`Catalog::pack_icons`] as
    /// `icon_layer - PACK_ICON_BASE`.
    pub icon_layer: u8,
}

/// A resolved shaped recipe definition.
#[derive(Clone, Debug, PartialEq)]
pub struct RecipeDef {
    /// Dense catalog index.
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
        block: u16,
        replace: u16,
        min_y: i32,
        max_y: i32,
        vein: u8,
        attempts: u8,
    },
    /// Surface patch replacing a host block inside listed biomes.
    Patch {
        block: u16,
        replace: u16,
        biomes: Vec<BiomeTag>,
        min_y: i32,
        max_y: i32,
        chance: f32,
    },
}

/// A resolved block definition, raw-typed.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockDef {
    /// Raw catalog id this definition describes.
    pub id: u16,
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

/// Immutable, neutral catalog of content definitions.
///
/// Every table is a dense vector indexed by the definition's own raw/index
/// field, and allocation is deterministic (sorted ids, dense indices,
/// append-only layers/icons), so an external consumer and the game agree on
/// ids, layers and digest for the same pack stack.
#[derive(Clone, Debug, PartialEq)]
pub struct Catalog {
    blocks: Vec<BlockDef>,
    items: Vec<ItemDef>,
    tiers: Vec<TierDef>,
    tools: Vec<ToolDef>,
    recipes: Vec<RecipeDef>,
    worldgen_rules: Vec<WorldgenRule>,
    pack_layers: Vec<Vec<u8>>,
    pack_icons: Vec<Vec<u8>>,
    pack_layer_sources: Vec<LayerSource>,
    pack_icon_sources: Vec<LayerSource>,
    packs: Vec<PackIdentity>,
    digest: u64,
}

impl Catalog {
    /// Builds a catalog whose only populated tables are the supplied base
    /// rows.
    ///
    /// Items, tools, worldgen rules and every pack table start empty and the
    /// digest is [`EMPTY_DIGEST`], exactly like a built-in-only registry:
    /// built-in definitions are outside the pack digest.
    pub fn from_base(
        blocks: Vec<BlockDef>,
        tiers: Vec<TierDef>,
        recipes: Vec<RecipeDef>,
    ) -> Catalog {
        Catalog {
            blocks,
            items: Vec::new(),
            tiers,
            tools: Vec::new(),
            recipes,
            worldgen_rules: Vec::new(),
            pack_layers: Vec::new(),
            pack_icons: Vec::new(),
            pack_layer_sources: Vec::new(),
            pack_icon_sources: Vec::new(),
            packs: Vec::new(),
            digest: EMPTY_DIGEST,
        }
    }

    /// Appends a deterministic placeholder block for a save-local name.
    ///
    /// The new definition's raw id is the current table length. The caller
    /// supplies the stack cap so the game keeps its own `MAX_STACK_COUNT`
    /// policy. Returns the allocated raw id.
    pub fn append_placeholder_block(&mut self, name: &str, stack_limit: u16) -> u16 {
        let index =
            u16::try_from(self.blocks.len()).expect("catalog block count exceeds the u16 id space");
        self.blocks
            .push(placeholder_block(index, name, stack_limit));
        index
    }

    /// Appends a deterministic placeholder item for a save-local name.
    ///
    /// The new definition's dense index is the current table length. Returns
    /// the allocated index.
    pub fn append_placeholder_item(&mut self, name: &str, stack_limit: u16) -> u16 {
        let index =
            u16::try_from(self.items.len()).expect("catalog item count exceeds the u16 item space");
        self.items.push(placeholder_item(index, name, stack_limit));
        index
    }

    /// Appends a deterministic placeholder for a pack tool whose pack is gone.
    ///
    /// Behaves like [`Self::append_placeholder_item`] but records
    /// `placeholder_tool`, so a re-save keeps the record on the pack-tool wire
    /// tag. Tools never stack, so the placeholder is count-one only.
    pub fn append_placeholder_tool_item(&mut self, name: &str) -> u16 {
        let index =
            u16::try_from(self.items.len()).expect("catalog item count exceeds the u16 item space");
        self.items.push(placeholder_tool_item(index, name));
        index
    }

    /// All registered block definitions in raw id order.
    pub fn blocks(&self) -> &[BlockDef] {
        &self.blocks
    }

    /// The block at raw `index`.
    pub fn block(&self, index: u16) -> Option<&BlockDef> {
        self.blocks.get(usize::from(index))
    }

    /// The block whose stable short name is exactly `name`.
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

    /// All resolved tool tiers: the built-in rows followed by pack tiers.
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

    /// All resolved recipes: the built-in rows followed by pack recipes.
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

    /// Worldgen rules in authored stack order (later packs last).
    pub fn worldgen_rules(&self) -> &[WorldgenRule] {
        &self.worldgen_rules
    }

    /// Pack-authored block face layers, index-aligned with
    /// `reported layer - PACK_LAYER_BASE`.
    pub fn pack_layers(&self) -> &[Vec<u8>] {
        &self.pack_layers
    }

    /// Pack-authored icons, index-aligned with
    /// `reported layer - PACK_ICON_BASE`.
    pub fn pack_icons(&self) -> &[Vec<u8>] {
        &self.pack_icons
    }

    /// Authored provenance for [`Self::pack_layers`], index-aligned with it.
    pub fn pack_layer_sources(&self) -> &[LayerSource] {
        &self.pack_layer_sources
    }

    /// Authored provenance for [`Self::pack_icons`], index-aligned with it.
    pub fn pack_icon_sources(&self) -> &[LayerSource] {
        &self.pack_icon_sources
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

/// The canonical placeholder block row for a save-local name.
///
/// Uses the reserved checker face layer for every face and authors no
/// hardness, drop or tool class, so placeholder art can never be granted or
/// mined into a live identity.
pub fn placeholder_block(index: u16, name: &str, stack_limit: u16) -> BlockDef {
    BlockDef {
        id: index,
        name: name.to_string(),
        display_name: name.to_string(),
        faces: [PLACEHOLDER_BLOCK_LAYER; 6],
        hardness: None,
        drop: None,
        tool_class: None,
        stack_limit,
        placeholder: true,
    }
}

/// The canonical placeholder item row for a save-local name.
///
/// Uses the reserved opaque checker icon and is never food.
pub fn placeholder_item(index: u16, name: &str, stack_limit: u16) -> ItemDef {
    ItemDef {
        index,
        name: name.to_string(),
        display_name: name.to_string(),
        icon_layer: PLACEHOLDER_ICON_LAYER,
        stack_limit,
        food: None,
        placeholder: true,
        placeholder_tool: false,
    }
}

/// The canonical placeholder row for a pack tool whose pack is gone.
///
/// Records `placeholder_tool` and is count-one only: a restored tool comes
/// back at its authored maximum durability rather than a saved wear value.
pub fn placeholder_tool_item(index: u16, name: &str) -> ItemDef {
    ItemDef {
        index,
        name: name.to_string(),
        display_name: name.to_string(),
        icon_layer: PLACEHOLDER_ICON_LAYER,
        stack_limit: 1,
        food: None,
        placeholder: true,
        placeholder_tool: true,
    }
}

/// Builds a readable, non-blocking note describing how `current_packs` differs
/// from the stack that wrote a save.
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
    current_packs: &[PackIdentity],
    current_digest: u64,
    unresolved: usize,
) -> Option<String> {
    let saved: BTreeMap<&str, Option<&str>> = saved_packs
        .iter()
        .map(|pack| (pack.folder.as_str(), pack.version.as_deref()))
        .collect();
    let loaded: BTreeMap<&str, Option<&str>> = current_packs
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
    if saved_digest != current_digest && notes.is_empty() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{ContentDiagnostic, Severity};
    use crate::vocabulary::BIOME_NAMES;

    fn identity(folder: &str, version: Option<&str>) -> PackIdentity {
        PackIdentity {
            folder: folder.to_string(),
            version: version.map(str::to_string),
        }
    }

    fn base_catalog() -> Catalog {
        Catalog::from_base(
            vec![
                BlockDef {
                    id: 0,
                    name: "air".to_string(),
                    display_name: "Air".to_string(),
                    faces: [0; 6],
                    hardness: None,
                    drop: None,
                    tool_class: None,
                    stack_limit: 0,
                    placeholder: false,
                },
                BlockDef {
                    id: 1,
                    name: "stone".to_string(),
                    display_name: "Stone".to_string(),
                    faces: [3; 6],
                    hardness: Some(1.5),
                    drop: Some(ItemRef::Block(1)),
                    tool_class: Some(ToolKind::Pickaxe),
                    stack_limit: 64,
                    placeholder: false,
                },
            ],
            vec![TierDef {
                index: 0,
                name: "wood".to_string(),
                display_name: "Wood".to_string(),
                speed: 1.0,
                durability: 59,
                damage: 3,
            }],
            vec![RecipeDef {
                index: 0,
                name: "planks".to_string(),
                pattern: vec!["X".to_string()],
                key: BTreeMap::new(),
                output: ItemRef::Block(1),
                count: 4,
                enabled: true,
                placeholder: false,
            }],
        )
    }

    #[test]
    fn from_base_keeps_only_the_supplied_rows() {
        let catalog = base_catalog();

        assert_eq!(catalog.blocks().len(), 2);
        assert_eq!(catalog.block(1).map(|def| def.name.as_str()), Some("stone"));
        assert_eq!(
            catalog.block_by_name("air").map(|def| def.id),
            Some(0),
            "raw id lookup and name lookup agree"
        );
        assert_eq!(catalog.block(2), None);
        assert_eq!(catalog.tiers().len(), 1);
        assert_eq!(catalog.tier(0).map(|def| def.speed), Some(1.0));
        assert_eq!(catalog.recipe(0).map(|def| def.count), Some(4));
        assert_eq!(
            catalog.recipe_by_name("planks").map(|def| def.index),
            Some(0)
        );
        assert!(catalog.items().is_empty());
        assert!(catalog.tools().is_empty());
        assert!(catalog.worldgen_rules().is_empty());
        assert!(catalog.pack_layers().is_empty());
        assert!(catalog.pack_icons().is_empty());
        assert!(catalog.pack_layer_sources().is_empty());
        assert!(catalog.pack_icon_sources().is_empty());
        assert!(catalog.packs().is_empty());
        assert_eq!(catalog.digest(), EMPTY_DIGEST);
    }

    #[test]
    fn placeholder_rows_allocate_from_the_current_table_length() {
        let mut catalog = base_catalog();
        let base_blocks = catalog.blocks().to_vec();

        let block = catalog.append_placeholder_block("save:missing_block", 999);
        assert_eq!(block, 2, "the raw id is the current block table length");
        let row = catalog.block(block).expect("placeholder block row");
        assert_eq!(row.id, 2);
        assert_eq!(row.name, "save:missing_block");
        assert_eq!(row.display_name, "save:missing_block");
        assert_eq!(row.faces, [PLACEHOLDER_BLOCK_LAYER; 6]);
        assert_eq!(row.hardness, None);
        assert_eq!(row.drop, None);
        assert_eq!(row.tool_class, None);
        assert_eq!(row.stack_limit, 999);
        assert!(row.placeholder);

        let item = catalog.append_placeholder_item("save:missing_item", 999);
        assert_eq!(item, 0, "the index is the current item table length");
        let row = catalog.item(item).expect("placeholder item row");
        assert_eq!(row.index, 0);
        assert_eq!(row.name, "save:missing_item");
        assert_eq!(row.display_name, "save:missing_item");
        assert_eq!(row.icon_layer, PLACEHOLDER_ICON_LAYER);
        assert_eq!(row.stack_limit, 999);
        assert_eq!(row.food, None);
        assert!(row.placeholder);
        assert!(!row.placeholder_tool);

        let tool = catalog.append_placeholder_tool_item("save:missing_tool");
        assert_eq!(tool, 1, "the tool placeholder follows the generic item");
        let row = catalog.item(tool).expect("placeholder tool row");
        assert_eq!(row.index, 1);
        assert_eq!(row.name, "save:missing_tool");
        assert_eq!(row.icon_layer, PLACEHOLDER_ICON_LAYER);
        assert_eq!(row.stack_limit, 1, "tools never stack");
        assert_eq!(row.food, None);
        assert!(row.placeholder);
        assert!(row.placeholder_tool);

        // Appending placeholders never touches the digest, the pack list or
        // the base rows: placeholder content is not pack content.
        assert_eq!(catalog.digest(), EMPTY_DIGEST);
        assert!(catalog.packs().is_empty());
        assert_eq!(
            &catalog.blocks()[..2],
            base_blocks.as_slice(),
            "base rows stay untouched"
        );
    }

    #[test]
    fn free_placeholder_rows_match_the_appended_rows() {
        assert_eq!(
            placeholder_block(7, "save:gem", 999),
            base_catalog_placeholder_block(7)
        );
        assert_eq!(
            placeholder_item(3, "save:gem", 999),
            ItemDef {
                index: 3,
                name: "save:gem".to_string(),
                display_name: "save:gem".to_string(),
                icon_layer: PLACEHOLDER_ICON_LAYER,
                stack_limit: 999,
                food: None,
                placeholder: true,
                placeholder_tool: false,
            }
        );
        let tool = placeholder_tool_item(4, "save:pickaxe");
        assert_eq!(tool.index, 4);
        assert_eq!(tool.stack_limit, 1);
        assert!(tool.placeholder && tool.placeholder_tool);
        assert_eq!(tool.food, None, "a restored tool placeholder is never food");
    }

    fn base_catalog_placeholder_block(index: u16) -> BlockDef {
        BlockDef {
            id: index,
            name: "save:gem".to_string(),
            display_name: "save:gem".to_string(),
            faces: [PLACEHOLDER_BLOCK_LAYER; 6],
            hardness: None,
            drop: None,
            tool_class: None,
            stack_limit: 999,
            placeholder: true,
        }
    }

    #[test]
    fn content_load_report_counts_by_severity() {
        let empty = ContentLoadReport::default();
        assert_eq!(empty.errors(), 0);
        assert_eq!(empty.warnings(), 0);

        let report = ContentLoadReport {
            entries: 3,
            diagnostics: vec![
                ContentDiagnostic {
                    severity: Severity::Warning,
                    entry: "block mymod:ruby_ore".to_string(),
                    reason: "shadowed".to_string(),
                },
                ContentDiagnostic {
                    severity: Severity::Error,
                    entry: "pack".to_string(),
                    reason: "unresolved".to_string(),
                },
                ContentDiagnostic {
                    severity: Severity::Warning,
                    entry: "asset ruby.png".to_string(),
                    reason: "missing".to_string(),
                },
            ],
        };
        assert_eq!(report.errors(), 1);
        assert_eq!(report.warnings(), 2);
    }

    #[test]
    fn biome_tags_follow_the_frozen_vocabulary_order() {
        assert_eq!(BIOME_NAMES, ["plains", "forest", "mountains", "ocean"]);
        for (index, name) in BIOME_NAMES.iter().enumerate() {
            let tag = BiomeTag::from_name(name).expect("every vocabulary biome has a tag");
            let expected = u8::try_from(index).expect("four biomes fit a u8");
            assert_eq!(tag.index(), expected);
            assert_eq!(BiomeTag::from_index(expected), Some(tag));
        }
        assert_eq!(BiomeTag::from_name("desert"), None);
        assert_eq!(BiomeTag::from_name("Plains"), None);
        assert_eq!(BiomeTag::from_index(4), None);
    }

    #[test]
    fn content_mismatch_note_is_none_for_matching_content() {
        assert_eq!(
            content_mismatch_note(&[], EMPTY_DIGEST, &[], EMPTY_DIGEST, 0),
            None
        );

        let packs = [identity("alpha", Some("1.0")), identity("beta", None)];
        assert_eq!(
            content_mismatch_note(&packs, 0x1234, &packs, 0x1234, 0),
            None
        );
    }

    #[test]
    fn content_mismatch_note_reports_missing_packs_and_placeholders() {
        let saved = [
            identity("alpha", Some("1.0")),
            identity("beta", Some("2.0")),
        ];
        let current = [identity("alpha", Some("1.0"))];

        let note = content_mismatch_note(&saved, 0x1111, &current, 0x2222, 2)
            .expect("a missing pack is a mismatch");
        assert_eq!(
            note,
            "missing pack `beta`; 2 saved content names are unavailable and render as placeholders"
        );
        assert!(
            !note.contains("content definitions changed"),
            "a pack-level difference explains the digest: {note}"
        );
    }

    #[test]
    fn content_mismatch_note_reports_added_packs_and_version_changes() {
        let saved = [identity("alpha", Some("1.0"))];

        let added = [identity("alpha", Some("1.0")), identity("beta", None)];
        assert_eq!(
            content_mismatch_note(&saved, 0x1111, &added, 0x2222, 0),
            Some("added pack `beta`".to_string())
        );

        let changed = [identity("alpha", Some("2.0"))];
        assert_eq!(
            content_mismatch_note(&saved, 0x1111, &changed, 0x2222, 0),
            Some("pack `alpha` version changed from 1.0 to 2.0".to_string())
        );

        let unspecified = [identity("alpha", None)];
        assert_eq!(
            content_mismatch_note(&saved, 0x1111, &unspecified, 0x2222, 0),
            Some("pack `alpha` version changed from 1.0 to unspecified".to_string())
        );
    }

    #[test]
    fn content_mismatch_note_reports_an_unexplained_digest_change() {
        let packs = [identity("alpha", Some("1.0"))];
        assert_eq!(
            content_mismatch_note(&packs, 0x1111, &packs, 0x2222, 0),
            Some("content definitions changed".to_string())
        );
    }

    #[test]
    fn content_mismatch_note_uses_singular_and_plural_placeholder_counts() {
        assert_eq!(
            content_mismatch_note(&[], EMPTY_DIGEST, &[], EMPTY_DIGEST, 1),
            Some("1 saved content name is unavailable and render as placeholders".to_string())
        );
        assert_eq!(
            content_mismatch_note(&[], EMPTY_DIGEST, &[], EMPTY_DIGEST, 3),
            Some("3 saved content names are unavailable and render as placeholders".to_string())
        );
    }

    #[test]
    fn placeholder_rows_are_deterministic_across_identical_catalogs() {
        let mut first = base_catalog();
        let mut second = base_catalog();
        assert_eq!(
            first.append_placeholder_tool_item("save:pickaxe"),
            second.append_placeholder_tool_item("save:pickaxe")
        );
        assert_eq!(first.items(), second.items());

        // A food placeholder would be wrong; assert the row shape stays pure.
        assert_eq!(first.item(0).and_then(|def| def.food.clone()), None);
    }
}
