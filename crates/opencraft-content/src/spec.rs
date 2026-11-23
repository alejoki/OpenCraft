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

/// Tool shape authored by a pack and consumed by the game's tool rules.
///
/// The neutral authored model owns the enum so a pack never names a game
/// type; the game keeps tier policy (`ToolTier`, durability and the
/// constructible pairs) on its side of the boundary. Declaration order is
/// frozen because the name tables and the demo digests are index-aligned to
/// it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ToolKind {
    Pickaxe,
    Axe,
    Shovel,
    Hoe,
    // Appended after Hoe; these are the trades the melee/bow slices need.
    Sword,
    Bow,
}

impl ToolKind {
    pub const fn name(self) -> &'static str {
        match self {
            ToolKind::Pickaxe => "Pickaxe",
            ToolKind::Axe => "Axe",
            ToolKind::Shovel => "Shovel",
            ToolKind::Hoe => "Hoe",
            ToolKind::Sword => "Sword",
            ToolKind::Bow => "Bow",
        }
    }
}

/// Render model authored for a block.
///
/// The neutral authored model owns the enum so a pack never names a render
/// type; the game maps it onto its own render class. The lowercase token is
/// the only spelling a pack may author.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum BlockModel {
    /// Full solid cube; the default for every block that omits `model`.
    #[default]
    Cube,
    /// Two crossed quads, used for plants such as tall grass and wheat.
    Cross,
    /// A cross plant whose growth stage selects the quad, for crops.
    Crop,
}

impl BlockModel {
    /// Exact lowercase token authored in `model =`.
    pub const fn as_str(self) -> &'static str {
        match self {
            BlockModel::Cube => "cube",
            BlockModel::Cross => "cross",
            BlockModel::Crop => "crop",
        }
    }

    /// Parse the exact lowercase token, or `None` for any other spelling.
    pub fn from_str(value: &str) -> Option<BlockModel> {
        match value {
            "cube" => Some(BlockModel::Cube),
            "cross" => Some(BlockModel::Cross),
            "crop" => Some(BlockModel::Crop),
            _ => None,
        }
    }
}

/// Alpha mode authored for a block's faces.
///
/// The neutral authored model owns the enum so the parser is the single place
/// an unknown token is rejected; the game maps it onto its own alpha class.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum BlockAlpha {
    /// Fully opaque faces; the default for every block that omits `alpha`.
    #[default]
    Opaque,
    /// Binary transparency: alpha below the cutout threshold is discarded.
    Cutout,
    /// Sorted translucency blended over the opaque scene.
    Blend,
}

impl BlockAlpha {
    /// Exact lowercase token authored in `alpha =`.
    pub const fn as_str(self) -> &'static str {
        match self {
            BlockAlpha::Opaque => "opaque",
            BlockAlpha::Cutout => "cutout",
            BlockAlpha::Blend => "blend",
        }
    }

    /// Parse the exact lowercase token, or `None` for any other spelling.
    pub fn from_str(value: &str) -> Option<BlockAlpha> {
        match value {
            "opaque" => Some(BlockAlpha::Opaque),
            "cutout" => Some(BlockAlpha::Cutout),
            "blend" => Some(BlockAlpha::Blend),
            _ => None,
        }
    }
}

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

/// Frozen face index → direction label.
///
/// Matches the game's `src/world/block.rs` and `prefab::Direction` order:
/// 0=+X/east, 1=-X/west, 2=+Y/top, 3=-Y/bottom, 4=+Z/south, 5=-Z/north.
pub const FACE_DIRECTIONS: [&str; 6] = ["east", "west", "top", "bottom", "south", "north"];

/// Direction label for a face index, or `None` when the index is out of range.
pub fn face_direction(face: usize) -> Option<&'static str> {
    FACE_DIRECTIONS.get(face).copied()
}

/// Where a built-in block face's texture bytes come from.
///
/// `Layer` means the bytes already live in a fixed base layer (for example a
/// legacy `<stem>_top.png` file that is still a built-in layer). `Authored`
/// names a pack-relative file that the loader decoded into
/// [`PackContentSpec::assets`] and that the catalog allocates at `48 + i`.
/// `None` on [`BuiltinFaceEntry::faces`] means keep the built-in
/// `face_tex_layer` default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FaceSource {
    Layer(u8),
    Authored(String),
}

/// One loader-synthesized per-direction face plan for a built-in block.
///
/// The neutral crate never synthesizes these itself: the game loader inspects
/// a pack folder and fills the table, so built-in blocks can point at new
/// per-direction PNGs without changing their raw ids.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct BuiltinFaceEntry {
    /// Built-in block short name, for example `stone`.
    pub block: String,
    /// Per-face sources in the frozen [`FACE_DIRECTIONS`] order.
    pub faces: [Option<FaceSource>; 6],
}

/// Everything a v3 pack authors, before registry allocation.
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
    /// Per-direction face overrides for built-in blocks, synthesized by the
    /// loader from the pack folder. Empty means every built-in block keeps its
    /// `face_tex_layer` default, so the plan never enters the digest until a
    /// new authored face file exists.
    pub builtin_faces: Vec<BuiltinFaceEntry>,
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
    /// Authored render model; `Cube` when the pack omits `model`.
    pub model: BlockModel,
    /// Authored alpha mode; `Opaque` when the pack omits `alpha`.
    pub alpha: BlockAlpha,
}

/// Authored block face texture file names.
///
/// Resolution precedence is fixed per face: the four sides (`east`, `west`,
/// `south`, `north`) fall back through `side` and then `all`; `top` and
/// `bottom` fall back directly to `all` and never consult `side`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockTextures {
    pub all: Option<String>,
    pub top: Option<String>,
    pub bottom: Option<String>,
    pub side: Option<String>,
    pub north: Option<String>,
    pub south: Option<String>,
    pub east: Option<String>,
    pub west: Option<String>,
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
    /// Patch over the built-in render model; `None` leaves it untouched.
    pub model: Option<BlockModel>,
    /// Patch over the built-in alpha mode; `None` leaves it untouched.
    pub alpha: Option<BlockAlpha>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_model_tokens_round_trip_and_reject_other_spellings() {
        for model in [BlockModel::Cube, BlockModel::Cross, BlockModel::Crop] {
            assert_eq!(BlockModel::from_str(model.as_str()), Some(model));
        }
        assert_eq!(
            [BlockModel::Cube, BlockModel::Cross, BlockModel::Crop].map(BlockModel::as_str),
            ["cube", "cross", "crop"]
        );
        for unknown in ["", "Cube", "CUBE", "column", "cross "] {
            assert_eq!(BlockModel::from_str(unknown), None, "{unknown:?}");
        }
        assert_eq!(BlockModel::default(), BlockModel::Cube);
    }

    #[test]
    fn block_alpha_tokens_round_trip_and_reject_other_spellings() {
        for alpha in [BlockAlpha::Opaque, BlockAlpha::Cutout, BlockAlpha::Blend] {
            assert_eq!(BlockAlpha::from_str(alpha.as_str()), Some(alpha));
        }
        assert_eq!(
            [BlockAlpha::Opaque, BlockAlpha::Cutout, BlockAlpha::Blend].map(BlockAlpha::as_str),
            ["opaque", "cutout", "blend"]
        );
        for unknown in ["", "Opaque", "translucent", "blend ", "cut_out"] {
            assert_eq!(BlockAlpha::from_str(unknown), None, "{unknown:?}");
        }
        assert_eq!(BlockAlpha::default(), BlockAlpha::Opaque);
    }
}
