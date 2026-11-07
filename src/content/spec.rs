//! Pure pack-content data model.
//!
//! These types are the parser's output and the registry builder's input. They
//! deliberately contain no filesystem, pack, settings, audio or GPU
//! dependency: content identity must stay decoupled from presentation.
//!
//! Every entry is the *accepted* form of an authored array entry. The pack
//! parser rejects an entry individually (recording a [`ContentDiagnostic`])
//! while keeping its valid siblings, so a spec never contains a half-parsed
//! definition.

use std::collections::BTreeMap;

use crate::player::inventory::ToolKind;

/// Severity of a pack-content diagnostic.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Severity {
    /// The entry was accepted, but something about it is worth surfacing.
    Warning,
    /// The entry (or the pack) was rejected.
    Error,
}

/// One structured problem found while parsing pack content.
///
/// `entry` is `"<kind> <id>"` for entry-scoped diagnostics (for example
/// `"block mymod:ruby_ore"`), `"asset <relative path>"` for asset-scoped
/// failures, or `"pack"` for pack-level problems. The field is stable so a
/// later `--validate-pack` can print it verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentDiagnostic {
    pub severity: Severity,
    pub entry: String,
    pub reason: String,
}

/// Everything a v2 pack authors, before registry allocation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackContentSpec {
    pub blocks: Vec<BlockEntry>,
    pub items: Vec<ItemEntry>,
    pub tiers: Vec<TierEntry>,
    pub tools: Vec<ToolEntry>,
    pub recipes: Vec<RecipeEntry>,
    pub worldgen: Vec<WorldgenEntry>,
    pub block_patches: Vec<BlockPatch>,
    pub recipe_patches: Vec<RecipePatch>,
    /// Built-in item sprite overrides: built-in material/tool short name to
    /// pack-relative authored PNG path.
    ///
    /// Presentation-only. This table is deliberately excluded from registry
    /// allocation and the content digest so adding or removing an override is
    /// a live visual edit, never a restart.
    pub builtin_icons: BTreeMap<String, String>,
    /// Resampled 16x16 RGBA assets keyed by pack-relative authored path.
    pub assets: BTreeMap<String, PackAsset>,
}

/// One authored block definition.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockEntry {
    /// Namespaced id, for example `mymod:ruby_ore`.
    pub id: String,
    /// Optional human-readable name; the registry derives one when absent.
    pub display_name: Option<String>,
    /// Authored face texture file names (relative to the pack folder).
    pub textures: BlockTextures,
    /// Seconds of held left-click needed to break.
    pub hardness: f32,
    /// Item id produced on break; `None` means the block itself.
    pub drop: Option<String>,
    /// Block class this block is efficiently broken with.
    pub tool: Option<ToolKind>,
    /// Maximum stack size of the block item.
    pub stack_limit: u16,
}

/// Authored block face texture file names.
///
/// `all` supplies every face; a specific face overrides `all` for that face.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockTextures {
    pub all: Option<String>,
    pub top: Option<String>,
    pub bottom: Option<String>,
    pub side: Option<String>,
}

/// One authored generic item (also the carrier of pack food data).
#[derive(Clone, Debug, PartialEq)]
pub struct ItemEntry {
    pub id: String,
    pub display_name: Option<String>,
    /// Icon file name relative to the pack folder.
    pub icon: String,
    pub stack_limit: u16,
    pub food: Option<FoodSpec>,
}

/// Food behaviour authored on an item.
#[derive(Clone, Debug, PartialEq)]
pub struct FoodSpec {
    /// Hunger points restored, 1..=20.
    pub nutrition: u16,
    /// Seconds the eat animation takes, 0.1..=10.0.
    pub eat_seconds: f32,
}

/// One authored tool tier.
#[derive(Clone, Debug, PartialEq)]
pub struct TierEntry {
    pub id: String,
    pub display_name: Option<String>,
    pub speed: f32,
    pub durability: u16,
    pub damage: u16,
}

/// One authored tool.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolEntry {
    pub id: String,
    pub display_name: Option<String>,
    pub kind: ToolKind,
    /// Built-in tier short name or a tier id defined in this pack.
    pub tier: String,
    /// Icon file name relative to the pack folder.
    pub icon: String,
}

/// One authored shaped recipe.
#[derive(Clone, Debug, PartialEq)]
pub struct RecipeEntry {
    pub id: String,
    pub pattern: Vec<String>,
    /// Pattern character to item reference.
    pub key: BTreeMap<char, String>,
    pub output: String,
    pub count: u16,
}

/// One authored worldgen rule.
#[derive(Clone, Debug, PartialEq)]
pub struct WorldgenEntry {
    pub kind: WorldgenKind,
}

/// Ore-vein or surface-patch rule content.
#[derive(Clone, Debug, PartialEq)]
pub enum WorldgenKind {
    Ore {
        block: String,
        replace: String,
        min_y: i32,
        max_y: i32,
        vein: u8,
        attempts: u8,
    },
    Patch {
        block: String,
        replace: String,
        biomes: Vec<String>,
        min_y: i32,
        max_y: i32,
        chance: f32,
    },
}

/// A per-field patch over a built-in block.
///
/// M010 patches can only set fields; a present field is `Some`, an absent one
/// is `None`, and no patch can clear a built-in value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlockPatch {
    /// Built-in block short name, for example `stone`.
    pub id: String,
    pub display_name: Option<String>,
    pub hardness: Option<f32>,
    pub drop: Option<String>,
    pub tool: Option<ToolKind>,
    pub stack_limit: Option<u16>,
}

/// Enable/disable a built-in recipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipePatch {
    /// Built-in recipe short name, for example `planks`.
    pub id: String,
    pub enabled: bool,
}

/// One decoded and resampled pack asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackAsset {
    /// Always 16x16 RGBA (1024 bytes).
    pub rgba: Vec<u8>,
    /// Authored side length before resampling.
    pub source_side: u32,
}
