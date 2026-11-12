//! v2 content-array parsing with per-entry diagnostics.
//!
//! A pack must never fail to load because one entry is wrong. Each authored
//! array entry is validated independently and either accepted into
//! [`PackContentSpec`] or rejected with a [`ContentDiagnostic`] naming
//! `"<kind> <id>"`; valid siblings are kept.
//!
//! Parsing is filesystem-free except for [`load_content_assets`], which reads
//! already-validated relative paths through [`crate::pack::asset`]. Cross-pack
//! references are deliberately unsupported in M010: every reference must
//! resolve to a built-in name or an entry in the same pack.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use crate::content::names;
use crate::content::spec::{
    BlockEntry, BlockPatch, BlockTextures, ContentDiagnostic, FoodSpec, ItemEntry, PackAsset,
    PackContentSpec, RecipeEntry, RecipePatch, Severity, TierEntry, ToolEntry, WorldgenEntry,
    WorldgenKind,
};
use crate::pack::asset;
use crate::pack::decode;
use crate::pack::manifest::PackFormat;
use crate::pack::resample;
use crate::pack::spec::{MAX_LAYER_SIDE, MIN_LAYER_SIDE};
use crate::player::inventory::ToolKind;
use crate::settings::SettingsWarning;

/// Maximum accepted entries in one authored array.
const MAX_ARRAY_ENTRIES: usize = 256;

/// Presentation-only table of built-in item sprite overrides.
const BUILTIN_ICON_TABLE: &str = "builtin_icons";

/// Arrays that only exist in format version 2.
const CONTENT_ARRAY_KEYS: [&str; 8] = [
    "blocks",
    "items",
    "tiers",
    "tools",
    "recipes",
    "worldgen",
    "block_patches",
    "recipe_patches",
];

/// Parse the v2 content arrays out of an already-parsed manifest root.
///
/// Version gating happens here:
/// - [`PackFormat::V1`] ignores content arrays entirely and emits exactly one
///   warning when any array is present.
/// - [`PackFormat::Future`] always warns once that the format is newer than
///   this build and ignores content; textures and sounds still load.
/// - [`PackFormat::V2`] parses every array.
///
/// Entry-scoped problems become [`ContentDiagnostic`] values; they never
/// propagate as an error.
pub fn parse_content(
    root: &toml::Value,
    format: PackFormat,
) -> (
    PackContentSpec,
    Vec<SettingsWarning>,
    Vec<ContentDiagnostic>,
) {
    let mut warnings = Vec::new();
    let mut diagnostics = Vec::new();

    match format {
        PackFormat::V1 => {
            if CONTENT_ARRAY_KEYS
                .iter()
                .any(|key| root.get(*key).is_some())
            {
                warnings.push(SettingsWarning {
                    field: "pack.toml".to_string(),
                    reason: "content arrays require format_version = 2".to_string(),
                });
            } else if root.get(BUILTIN_ICON_TABLE).is_some() {
                // Separate text from the array warning so existing pinned
                // messages stay byte-identical; a pack with both warns once.
                warnings.push(SettingsWarning {
                    field: "pack.toml".to_string(),
                    reason: "builtin icon overrides require format_version = 2".to_string(),
                });
            }
            return (PackContentSpec::default(), warnings, diagnostics);
        }
        PackFormat::Future(version) => {
            warnings.push(SettingsWarning {
                field: "pack.toml".to_string(),
                reason: format!(
                    "format_version {version} is newer than 2; content arrays are ignored"
                ),
            });
            return (PackContentSpec::default(), warnings, diagnostics);
        }
        PackFormat::V2 => {}
    }

    let mut spec = PackContentSpec::default();

    // Blocks, items and tools share one content-id namespace. Tiers and
    // recipes each have an independent namespace.
    let mut content_ids: HashSet<String> = HashSet::new();
    let mut tier_ids: HashSet<String> = HashSet::new();
    let mut recipe_ids: HashSet<String> = HashSet::new();
    let mut block_patch_ids: HashSet<String> = HashSet::new();
    let mut recipe_patch_ids: HashSet<String> = HashSet::new();

    for (index, value) in array_entries(root, "blocks", &mut diagnostics)
        .into_iter()
        .enumerate()
    {
        if let Some(entry) = parse_block(value, index, &mut content_ids, &mut diagnostics) {
            spec.blocks.push(entry);
        }
    }
    for (index, value) in array_entries(root, "items", &mut diagnostics)
        .into_iter()
        .enumerate()
    {
        if let Some(entry) = parse_item(value, index, &mut content_ids, &mut diagnostics) {
            spec.items.push(entry);
        }
    }
    for (index, value) in array_entries(root, "tiers", &mut diagnostics)
        .into_iter()
        .enumerate()
    {
        if let Some(entry) = parse_tier(value, index, &mut tier_ids, &mut diagnostics) {
            spec.tiers.push(entry);
        }
    }
    for (index, value) in array_entries(root, "tools", &mut diagnostics)
        .into_iter()
        .enumerate()
    {
        if let Some(entry) = parse_tool(value, index, &mut content_ids, &mut diagnostics) {
            spec.tools.push(entry);
        }
    }
    for (index, value) in array_entries(root, "recipes", &mut diagnostics)
        .into_iter()
        .enumerate()
    {
        if let Some(entry) = parse_recipe(value, index, &mut recipe_ids, &mut diagnostics) {
            spec.recipes.push(entry);
        }
    }
    for (index, value) in array_entries(root, "worldgen", &mut diagnostics)
        .into_iter()
        .enumerate()
    {
        if let Some(entry) = parse_worldgen(value, index, &mut diagnostics) {
            spec.worldgen.push(entry);
        }
    }
    for (index, value) in array_entries(root, "block_patches", &mut diagnostics)
        .into_iter()
        .enumerate()
    {
        if let Some(entry) = parse_block_patch(value, index, &mut block_patch_ids, &mut diagnostics)
        {
            spec.block_patches.push(entry);
        }
    }
    for (index, value) in array_entries(root, "recipe_patches", &mut diagnostics)
        .into_iter()
        .enumerate()
    {
        if let Some(entry) =
            parse_recipe_patch(value, index, &mut recipe_patch_ids, &mut diagnostics)
        {
            spec.recipe_patches.push(entry);
        }
    }

    parse_builtin_icons(root, &mut spec, &mut diagnostics);

    validate_references(&mut spec, &mut diagnostics);

    (spec, warnings, diagnostics)
}

/// Read assets referenced by accepted entries, in sorted path order.
///
/// Every unique authored path is loaded once. Failures (missing file, path
/// escape, byte cap, decode error, wrong dimensions) add an `"asset <path>"`
/// error and leave the asset absent; the entry itself stays in the spec so the
/// registry can surface the same diagnostic. A cumulative byte overflow adds
/// one `"pack"` error and stops loading further assets.
pub fn load_content_assets(
    pack_dir: &Path,
    spec: &mut PackContentSpec,
    diagnostics: &mut Vec<ContentDiagnostic>,
) {
    let paths: Vec<String> = collect_asset_paths(spec)
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut total_bytes: u64 = 0;

    for relative in paths {
        if spec.assets.contains_key(&relative) {
            continue;
        }

        let bytes = match asset::read_asset_bytes(pack_dir, &relative) {
            Ok((_path, bytes)) => bytes,
            Err(reason) => {
                err(diagnostics, format!("asset {relative}"), reason);
                continue;
            }
        };

        total_bytes = total_bytes.saturating_add(bytes.len() as u64);
        if total_bytes > asset::PACK_TOTAL_ASSET_BYTES_CAP {
            err(
                diagnostics,
                "pack",
                format!(
                    "pack assets exceed the {} byte total limit; remaining assets were not loaded",
                    asset::PACK_TOTAL_ASSET_BYTES_CAP
                ),
            );
            break;
        }

        let decoded = match decode::decode_png_rgba_bytes(&bytes, &relative, Some(MAX_LAYER_SIDE)) {
            Ok(decoded) => decoded,
            Err(reason) => {
                err(diagnostics, format!("asset {relative}"), reason);
                continue;
            }
        };

        if decoded.width != decoded.height {
            err(
                diagnostics,
                format!("asset {relative}"),
                format!(
                    "image is {}x{}, assets must be square",
                    decoded.width, decoded.height
                ),
            );
            continue;
        }

        if decoded.width < MIN_LAYER_SIDE || decoded.width > MAX_LAYER_SIDE {
            err(
                diagnostics,
                format!("asset {relative}"),
                format!(
                    "image is {}x{}, assets must be between {} and {} pixels per side",
                    decoded.width, decoded.height, MIN_LAYER_SIDE, MAX_LAYER_SIDE
                ),
            );
            continue;
        }

        spec.assets.insert(
            relative,
            PackAsset {
                rgba: resample::resample_to_16(&decoded.rgba, decoded.width),
                source_side: decoded.width,
            },
        );
    }
}

/// Unique authored asset paths in deterministic (sorted) order.
fn collect_asset_paths(spec: &PackContentSpec) -> BTreeSet<&str> {
    let mut paths = BTreeSet::new();

    for block in &spec.blocks {
        for texture in [
            &block.textures.all,
            &block.textures.top,
            &block.textures.bottom,
            &block.textures.side,
        ]
        .into_iter()
        .flatten()
        {
            paths.insert(texture.as_str());
        }
    }
    for item in &spec.items {
        paths.insert(item.icon.as_str());
    }
    for tool in &spec.tools {
        paths.insert(tool.icon.as_str());
    }
    paths.extend(spec.builtin_icons.values().map(String::as_str));

    paths
}

// ---------------------------------------------------------------------------
// Built-in item sprite overrides
// ---------------------------------------------------------------------------

/// Parse the presentation-only `[builtin_icons]` table.
///
/// Every accepted entry is a built-in material/tool short name mapped to a
/// pack-relative authored PNG path. The table never feeds registry allocation
/// or the content digest: it is validated here, its paths join the normal v2
/// asset pipeline, and presentation applies the loaded bytes later.
fn parse_builtin_icons(
    root: &toml::Value,
    spec: &mut PackContentSpec,
    diagnostics: &mut Vec<ContentDiagnostic>,
) {
    let Some(value) = root.get(BUILTIN_ICON_TABLE) else {
        return;
    };
    let Some(table) = value.as_table() else {
        err(
            diagnostics,
            BUILTIN_ICON_TABLE,
            format!(
                "expected `[{BUILTIN_ICON_TABLE}]` to be a table, found {}",
                value.type_str()
            ),
        );
        return;
    };

    // One cap diagnostic for the whole table. Rejecting the table wholesale
    // keeps the message count bounded even for a hostile manifest.
    if table.len() > MAX_ARRAY_ENTRIES {
        err(
            diagnostics,
            BUILTIN_ICON_TABLE,
            format!("table exceeds the {MAX_ARRAY_ENTRIES}-entry limit for [{BUILTIN_ICON_TABLE}]"),
        );
        return;
    }

    for (name, value) in table {
        let entry = format!("builtin_icon {name}");
        if names::builtin_item_sprite_id_by_name(name).is_none() {
            let reason = if names::builtin_block_id_by_name(name).is_some() {
                format!(
                    "`{name}` is a block; builtin icon overrides target material and tool items"
                )
            } else {
                format!("`{name}` is not a builtin material or tool")
            };
            err(diagnostics, entry, reason);
            continue;
        }
        let Some(path) = value.as_str() else {
            err(
                diagnostics,
                entry,
                format!("expected `path` to be a string, found {}", value.type_str()),
            );
            continue;
        };
        if validate_asset_path(path, "path", &entry, diagnostics).is_err() {
            continue;
        }
        spec.builtin_icons.insert(name.clone(), path.to_string());
    }
}

// ---------------------------------------------------------------------------
// Entry parsing
// ---------------------------------------------------------------------------

type Parsed<T> = Result<T, ()>;

fn err(
    diagnostics: &mut Vec<ContentDiagnostic>,
    entry: impl Into<String>,
    reason: impl Into<String>,
) {
    diagnostics.push(ContentDiagnostic {
        severity: Severity::Error,
        entry: entry.into(),
        reason: reason.into(),
    });
}

/// Return every element of `root[key]`, or one pack-level diagnostic when the
/// key exists but is not an array.
fn array_entries<'a>(
    root: &'a toml::Value,
    key: &str,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Vec<&'a toml::Value> {
    let Some(value) = root.get(key) else {
        return Vec::new();
    };
    match value.as_array() {
        Some(array) => array.iter().collect(),
        None => {
            err(
                diagnostics,
                "pack",
                format!(
                    "expected `[[{key}]]` to be an array of tables, found {}",
                    value.type_str()
                ),
            );
            Vec::new()
        }
    }
}

fn within_cap(
    index: usize,
    array: &str,
    entry: &str,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> bool {
    if index < MAX_ARRAY_ENTRIES {
        return true;
    }
    err(
        diagnostics,
        entry,
        format!(
            "entry {} exceeds the {MAX_ARRAY_ENTRIES}-entry limit for [[{array}]]",
            index + 1
        ),
    );
    false
}

fn entry_label(kind: &str, table: &toml::Table, index: usize) -> String {
    match table.get("id").and_then(|value| value.as_str()) {
        Some(id) => format!("{kind} {id}"),
        None => format!("{kind} #{}", index + 1),
    }
}

fn required_string(
    table: &toml::Table,
    key: &str,
    entry: &str,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Parsed<String> {
    match table.get(key) {
        Some(toml::Value::String(value)) => Ok(value.clone()),
        Some(value) => {
            err(
                diagnostics,
                entry,
                format!(
                    "expected `{key}` to be a string, found {}",
                    value.type_str()
                ),
            );
            Err(())
        }
        None => {
            err(
                diagnostics,
                entry,
                format!("missing required field `{key}`"),
            );
            Err(())
        }
    }
}

fn optional_string(
    table: &toml::Table,
    key: &str,
    entry: &str,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Parsed<Option<String>> {
    let Some(value) = table.get(key) else {
        return Ok(None);
    };
    match value.as_str() {
        Some(value) => Ok(Some(value.to_string())),
        None => {
            err(
                diagnostics,
                entry,
                format!(
                    "expected `{key}` to be a string, found {}",
                    value.type_str()
                ),
            );
            Err(())
        }
    }
}

fn number_value(value: &toml::Value) -> Option<f64> {
    value
        .as_float()
        .or_else(|| value.as_integer().map(|integer| integer as f64))
}

fn required_number(
    table: &toml::Table,
    key: &str,
    entry: &str,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Parsed<f64> {
    let Some(value) = table.get(key) else {
        err(
            diagnostics,
            entry,
            format!("missing required field `{key}`"),
        );
        return Err(());
    };
    let Some(number) = number_value(value) else {
        err(
            diagnostics,
            entry,
            format!(
                "expected `{key}` to be a number, found {}",
                value.type_str()
            ),
        );
        return Err(());
    };
    if !number.is_finite() {
        err(
            diagnostics,
            entry,
            format!("`{key}` must be a finite number, found {number}"),
        );
        return Err(());
    }
    Ok(number)
}

fn required_integer(
    table: &toml::Table,
    key: &str,
    entry: &str,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Parsed<i64> {
    let Some(value) = table.get(key) else {
        err(
            diagnostics,
            entry,
            format!("missing required field `{key}`"),
        );
        return Err(());
    };
    match value.as_integer() {
        Some(integer) => Ok(integer),
        None => {
            err(
                diagnostics,
                entry,
                format!(
                    "expected `{key}` to be an integer, found {}",
                    value.type_str()
                ),
            );
            Err(())
        }
    }
}

fn optional_integer(
    table: &toml::Table,
    key: &str,
    entry: &str,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Parsed<Option<i64>> {
    let Some(value) = table.get(key) else {
        return Ok(None);
    };
    match value.as_integer() {
        Some(integer) => Ok(Some(integer)),
        None => {
            err(
                diagnostics,
                entry,
                format!(
                    "expected `{key}` to be an integer, found {}",
                    value.type_str()
                ),
            );
            Err(())
        }
    }
}

fn optional_number(
    table: &toml::Table,
    key: &str,
    entry: &str,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Parsed<Option<f64>> {
    let Some(value) = table.get(key) else {
        return Ok(None);
    };
    let Some(number) = number_value(value) else {
        err(
            diagnostics,
            entry,
            format!(
                "expected `{key}` to be a number, found {}",
                value.type_str()
            ),
        );
        return Err(());
    };
    if !number.is_finite() {
        err(
            diagnostics,
            entry,
            format!("`{key}` must be a finite number, found {number}"),
        );
        return Err(());
    }
    Ok(Some(number))
}

fn required_bool(
    table: &toml::Table,
    key: &str,
    entry: &str,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Parsed<bool> {
    let Some(value) = table.get(key) else {
        err(
            diagnostics,
            entry,
            format!("missing required field `{key}`"),
        );
        return Err(());
    };
    match value.as_bool() {
        Some(boolean) => Ok(boolean),
        None => {
            err(
                diagnostics,
                entry,
                format!(
                    "expected `{key}` to be a boolean, found {}",
                    value.type_str()
                ),
            );
            Err(())
        }
    }
}

fn integer_in(
    table: &toml::Table,
    key: &str,
    min: i64,
    max: i64,
    default: i64,
    entry: &str,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Parsed<i64> {
    let integer = optional_integer(table, key, entry, diagnostics)?.unwrap_or(default);
    if integer < min || integer > max {
        err(
            diagnostics,
            entry,
            format!("`{key}` must be in {min}..={max}, found {integer}"),
        );
        return Err(());
    }
    Ok(integer)
}

fn required_integer_in(
    table: &toml::Table,
    key: &str,
    min: i64,
    max: i64,
    entry: &str,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Parsed<i64> {
    let integer = required_integer(table, key, entry, diagnostics)?;
    if integer < min || integer > max {
        err(
            diagnostics,
            entry,
            format!("`{key}` must be in {min}..={max}, found {integer}"),
        );
        return Err(());
    }
    Ok(integer)
}

fn required_number_in(
    table: &toml::Table,
    key: &str,
    min: f64,
    max: f64,
    entry: &str,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Parsed<f64> {
    let number = required_number(table, key, entry, diagnostics)?;
    if !(min..=max).contains(&number) {
        err(
            diagnostics,
            entry,
            format!("`{key}` must be in {min}..={max}, found {number}"),
        );
        return Err(());
    }
    Ok(number)
}

fn optional_number_in(
    table: &toml::Table,
    key: &str,
    min: f64,
    max: f64,
    default: f64,
    entry: &str,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Parsed<f64> {
    let number = optional_number(table, key, entry, diagnostics)?.unwrap_or(default);
    if !(min..=max).contains(&number) {
        err(
            diagnostics,
            entry,
            format!("`{key}` must be in {min}..={max}, found {number}"),
        );
        return Err(());
    }
    Ok(number)
}

fn validate_asset_path(
    path: &str,
    field: &str,
    entry: &str,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Parsed<()> {
    match asset::validate_relative_asset_path(path) {
        Ok(()) => Ok(()),
        Err(reason) => {
            err(diagnostics, entry, format!("`{field}` {reason}"));
            Err(())
        }
    }
}

fn validate_content_id(id: &str, entry: &str, diagnostics: &mut Vec<ContentDiagnostic>) -> bool {
    if !names::is_namespaced_id(id) {
        err(
            diagnostics,
            entry,
            format!(
                "`{id}` is not a namespaced id (expected `namespace:name` with lowercase letters, digits and `_`)"
            ),
        );
        return false;
    }
    if id.len() > names::MAX_CONTENT_NAME_LEN {
        err(
            diagnostics,
            entry,
            format!(
                "`{id}` exceeds the {}-byte content name limit",
                names::MAX_CONTENT_NAME_LEN
            ),
        );
        return false;
    }
    let namespace = id.split(':').next().unwrap_or("");
    if names::is_reserved_namespace(namespace) {
        err(
            diagnostics,
            entry,
            format!("namespace `{namespace}` is reserved for built-in content"),
        );
        return false;
    }
    true
}

fn parse_block(
    value: &toml::Value,
    index: usize,
    content_ids: &mut HashSet<String>,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Option<BlockEntry> {
    let Some(table) = value.as_table() else {
        err(
            diagnostics,
            format!("block #{}", index + 1),
            format!("expected a table, found {}", value.type_str()),
        );
        return None;
    };
    let entry = entry_label("block", table, index);
    if !within_cap(index, "blocks", &entry, diagnostics) {
        return None;
    }

    let id = required_string(table, "id", &entry, diagnostics).ok()?;
    if !validate_content_id(&id, &entry, diagnostics) {
        return None;
    }
    let display_name = optional_string(table, "name", &entry, diagnostics).ok()?;
    let textures = parse_block_textures(table, &entry, diagnostics).ok()?;
    let hardness = required_number_in(table, "hardness", 0.0, 3600.0, &entry, diagnostics)
        .ok()
        .map(|number| number as f32)?;
    let drop = optional_string(table, "drop", &entry, diagnostics).ok()?;
    let tool = parse_block_tool(table, &entry, diagnostics).ok()?;
    let stack_limit = integer_in(table, "stack", 1, 999, 999, &entry, diagnostics).ok()? as u16;
    if !content_ids.insert(id.clone()) {
        err(
            diagnostics,
            entry,
            format!("duplicate content id `{id}`; the first definition wins"),
        );
        return None;
    }

    Some(BlockEntry {
        id,
        display_name,
        textures,
        hardness,
        drop,
        tool,
        stack_limit,
    })
}

fn parse_block_textures(
    table: &toml::Table,
    entry: &str,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Parsed<BlockTextures> {
    let Some(value) = table.get("textures") else {
        err(
            diagnostics,
            entry,
            "missing required field `textures`".to_string(),
        );
        return Err(());
    };
    let Some(textures) = value.as_table() else {
        err(
            diagnostics,
            entry,
            format!(
                "expected `textures` to be a table, found {}",
                value.type_str()
            ),
        );
        return Err(());
    };

    let block_textures = BlockTextures {
        all: optional_string(textures, "all", entry, diagnostics)?,
        top: optional_string(textures, "top", entry, diagnostics)?,
        bottom: optional_string(textures, "bottom", entry, diagnostics)?,
        side: optional_string(textures, "side", entry, diagnostics)?,
    };

    if block_textures.all.is_none()
        && block_textures.top.is_none()
        && block_textures.bottom.is_none()
        && block_textures.side.is_none()
    {
        err(
            diagnostics,
            entry,
            "`textures` must set at least one of `all`, `top`, `bottom`, `side`",
        );
        return Err(());
    }

    for (field, path) in [
        ("textures.all", &block_textures.all),
        ("textures.top", &block_textures.top),
        ("textures.bottom", &block_textures.bottom),
        ("textures.side", &block_textures.side),
    ] {
        if let Some(path) = path {
            validate_asset_path(path, field, entry, diagnostics)?;
        }
    }

    Ok(block_textures)
}

fn parse_block_tool(
    table: &toml::Table,
    entry: &str,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Parsed<Option<ToolKind>> {
    let Some(name) = optional_string(table, "tool", entry, diagnostics)? else {
        return Ok(None);
    };
    match name.as_str() {
        "pickaxe" => Ok(Some(ToolKind::Pickaxe)),
        "axe" => Ok(Some(ToolKind::Axe)),
        "shovel" => Ok(Some(ToolKind::Shovel)),
        "hoe" => Ok(Some(ToolKind::Hoe)),
        "sword" | "bow" => {
            err(
                diagnostics,
                entry,
                format!(
                    "`tool` must be one of pickaxe, axe, shovel, hoe; `{name}` is not a block accelerator"
                ),
            );
            Err(())
        }
        other => {
            err(
                diagnostics,
                entry,
                format!("unknown block `tool` `{other}`"),
            );
            Err(())
        }
    }
}

fn parse_item(
    value: &toml::Value,
    index: usize,
    content_ids: &mut HashSet<String>,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Option<ItemEntry> {
    let Some(table) = value.as_table() else {
        err(
            diagnostics,
            format!("item #{}", index + 1),
            format!("expected a table, found {}", value.type_str()),
        );
        return None;
    };
    let entry = entry_label("item", table, index);
    if !within_cap(index, "items", &entry, diagnostics) {
        return None;
    }

    let id = required_string(table, "id", &entry, diagnostics).ok()?;
    if !validate_content_id(&id, &entry, diagnostics) {
        return None;
    }
    let display_name = optional_string(table, "name", &entry, diagnostics).ok()?;
    let icon = required_string(table, "icon", &entry, diagnostics).ok()?;
    validate_asset_path(&icon, "icon", &entry, diagnostics).ok()?;
    let stack_limit = integer_in(table, "stack", 1, 999, 64, &entry, diagnostics).ok()? as u16;

    let nutrition = optional_integer(table, "nutrition", &entry, diagnostics)
        .ok()?
        .map(|nutrition| {
            if !(1..=20).contains(&nutrition) {
                err(
                    diagnostics,
                    entry.clone(),
                    format!("`nutrition` must be in 1..=20, found {nutrition}"),
                );
                Err(())
            } else {
                Ok(nutrition as u16)
            }
        })
        .transpose()
        .ok()?;

    let food = match nutrition {
        Some(nutrition) => {
            let eat_seconds =
                optional_number_in(table, "eat_seconds", 0.1, 10.0, 1.2, &entry, diagnostics)
                    .ok()? as f32;
            Some(FoodSpec {
                nutrition,
                eat_seconds,
            })
        }
        None => None,
    };

    if !content_ids.insert(id.clone()) {
        err(
            diagnostics,
            entry,
            format!("duplicate content id `{id}`; the first definition wins"),
        );
        return None;
    }

    Some(ItemEntry {
        id,
        display_name,
        icon,
        stack_limit,
        food,
    })
}

fn parse_tier(
    value: &toml::Value,
    index: usize,
    tier_ids: &mut HashSet<String>,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Option<TierEntry> {
    let Some(table) = value.as_table() else {
        err(
            diagnostics,
            format!("tier #{}", index + 1),
            format!("expected a table, found {}", value.type_str()),
        );
        return None;
    };
    let entry = entry_label("tier", table, index);
    if !within_cap(index, "tiers", &entry, diagnostics) {
        return None;
    }

    let id = required_string(table, "id", &entry, diagnostics).ok()?;
    if !validate_content_id(&id, &entry, diagnostics) {
        return None;
    }
    let display_name = optional_string(table, "name", &entry, diagnostics).ok()?;
    let speed = required_number_in(table, "speed", 1.0, 100.0, &entry, diagnostics).ok()? as f32;
    let durability =
        required_integer_in(table, "durability", 1, 65_535, &entry, diagnostics).ok()? as u16;
    let damage = required_integer_in(table, "damage", 0, 65_535, &entry, diagnostics).ok()? as u16;
    if !tier_ids.insert(id.clone()) {
        err(
            diagnostics,
            entry,
            format!("duplicate tier id `{id}`; the first definition wins"),
        );
        return None;
    }

    Some(TierEntry {
        id,
        display_name,
        speed,
        durability,
        damage,
    })
}

fn parse_tool(
    value: &toml::Value,
    index: usize,
    content_ids: &mut HashSet<String>,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Option<ToolEntry> {
    let Some(table) = value.as_table() else {
        err(
            diagnostics,
            format!("tool #{}", index + 1),
            format!("expected a table, found {}", value.type_str()),
        );
        return None;
    };
    let entry = entry_label("tool", table, index);
    if !within_cap(index, "tools", &entry, diagnostics) {
        return None;
    }

    let id = required_string(table, "id", &entry, diagnostics).ok()?;
    if !validate_content_id(&id, &entry, diagnostics) {
        return None;
    }
    let display_name = optional_string(table, "name", &entry, diagnostics).ok()?;
    let kind_name = required_string(table, "kind", &entry, diagnostics).ok()?;
    let kind = match kind_name.as_str() {
        "pickaxe" => ToolKind::Pickaxe,
        "axe" => ToolKind::Axe,
        "shovel" => ToolKind::Shovel,
        "hoe" => ToolKind::Hoe,
        "sword" => ToolKind::Sword,
        "bow" => {
            err(
                diagnostics,
                entry,
                "`kind = \"bow\"` is not authorable in format v2".to_string(),
            );
            return None;
        }
        other => {
            err(diagnostics, entry, format!("unknown tool `kind` `{other}`"));
            return None;
        }
    };
    let tier = required_string(table, "tier", &entry, diagnostics).ok()?;
    let icon = required_string(table, "icon", &entry, diagnostics).ok()?;
    validate_asset_path(&icon, "icon", &entry, diagnostics).ok()?;
    if !content_ids.insert(id.clone()) {
        err(
            diagnostics,
            entry,
            format!("duplicate content id `{id}`; the first definition wins"),
        );
        return None;
    }

    Some(ToolEntry {
        id,
        display_name,
        kind,
        tier,
        icon,
    })
}

fn parse_recipe(
    value: &toml::Value,
    index: usize,
    recipe_ids: &mut HashSet<String>,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Option<RecipeEntry> {
    let Some(table) = value.as_table() else {
        err(
            diagnostics,
            format!("recipe #{}", index + 1),
            format!("expected a table, found {}", value.type_str()),
        );
        return None;
    };
    let entry = entry_label("recipe", table, index);
    if !within_cap(index, "recipes", &entry, diagnostics) {
        return None;
    }

    let id = required_string(table, "id", &entry, diagnostics).ok()?;
    if !validate_content_id(&id, &entry, diagnostics) {
        return None;
    }
    let Some(pattern_value) = table.get("pattern") else {
        err(
            diagnostics,
            entry,
            "missing required field `pattern`".to_string(),
        );
        return None;
    };
    let Some(rows) = pattern_value.as_array() else {
        err(
            diagnostics,
            entry,
            format!(
                "expected `pattern` to be an array of strings, found {}",
                pattern_value.type_str()
            ),
        );
        return None;
    };
    if rows.is_empty() || rows.len() > 4 {
        err(
            diagnostics,
            entry,
            format!("`pattern` must have 1..=4 rows, found {}", rows.len()),
        );
        return None;
    }
    let mut pattern = Vec::with_capacity(rows.len());
    for (row_index, row) in rows.iter().enumerate() {
        let Some(row) = row.as_str() else {
            err(
                diagnostics,
                entry,
                format!(
                    "expected `pattern[{row_index}]` to be a string, found {}",
                    row.type_str()
                ),
            );
            return None;
        };
        let width = row.chars().count();
        if width == 0 || width > 4 {
            err(
                diagnostics,
                entry,
                format!("`pattern[{row_index}]` must be 1..=4 characters, found {width}"),
            );
            return None;
        }
        pattern.push(row.to_string());
    }

    let Some(key_value) = table.get("key") else {
        err(
            diagnostics,
            entry,
            "missing required field `key`".to_string(),
        );
        return None;
    };
    let Some(key_table) = key_value.as_table() else {
        err(
            diagnostics,
            entry,
            format!(
                "expected `key` to be a table, found {}",
                key_value.type_str()
            ),
        );
        return None;
    };
    let mut key = BTreeMap::new();
    for (character, item) in key_table {
        let mut chars = character.chars();
        let Some(character) = chars.next() else {
            err(
                diagnostics,
                entry,
                "`key` contains an empty character key".to_string(),
            );
            return None;
        };
        if chars.next().is_some() {
            err(
                diagnostics,
                entry,
                format!("`key` entry `{character}` must be a single character"),
            );
            return None;
        }
        let Some(item) = item.as_str() else {
            err(
                diagnostics,
                entry,
                format!(
                    "expected `key.{character}` to be an item id string, found {}",
                    item.type_str()
                ),
            );
            return None;
        };
        key.insert(character, item.to_string());
    }

    let output = required_string(table, "output", &entry, diagnostics).ok()?;
    let count = required_integer_in(table, "count", 1, 999, &entry, diagnostics).ok()? as u16;
    if !recipe_ids.insert(id.clone()) {
        err(
            diagnostics,
            entry,
            format!("duplicate recipe id `{id}`; the first definition wins"),
        );
        return None;
    }

    Some(RecipeEntry {
        id,
        pattern,
        key,
        output,
        count,
    })
}

fn worldgen_label(table: &toml::Table, index: usize) -> String {
    let kind = table
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("entry");
    match table.get("block").and_then(|value| value.as_str()) {
        Some(block) => format!("worldgen {kind} {block}"),
        None => format!("worldgen {kind} #{}", index + 1),
    }
}

fn parse_worldgen(
    value: &toml::Value,
    index: usize,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Option<WorldgenEntry> {
    let Some(table) = value.as_table() else {
        err(
            diagnostics,
            format!("worldgen #{}", index + 1),
            format!("expected a table, found {}", value.type_str()),
        );
        return None;
    };
    let entry = worldgen_label(table, index);
    if !within_cap(index, "worldgen", &entry, diagnostics) {
        return None;
    }

    let kind_name = required_string(table, "type", &entry, diagnostics).ok()?;
    let block = required_string(table, "block", &entry, diagnostics).ok()?;
    let replace = required_string(table, "replace", &entry, diagnostics).ok()?;
    let min_y = required_integer_in(table, "min_y", -64, 128, &entry, diagnostics).ok()? as i32;
    let max_y = required_integer_in(table, "max_y", -64, 128, &entry, diagnostics).ok()? as i32;
    if min_y >= max_y {
        err(
            diagnostics,
            entry,
            format!("`min_y` ({min_y}) must be less than `max_y` ({max_y})"),
        );
        return None;
    }

    let kind = match kind_name.as_str() {
        "ore" => {
            let vein = required_integer_in(table, "vein", 1, 8, &entry, diagnostics).ok()? as u8;
            let attempts =
                required_integer_in(table, "attempts", 1, 32, &entry, diagnostics).ok()? as u8;
            WorldgenKind::Ore {
                block,
                replace,
                min_y,
                max_y,
                vein,
                attempts,
            }
        }
        "patch" => {
            let Some(biomes_value) = table.get("biomes") else {
                err(
                    diagnostics,
                    entry,
                    "missing required field `biomes`".to_string(),
                );
                return None;
            };
            let Some(biome_rows) = biomes_value.as_array() else {
                err(
                    diagnostics,
                    entry,
                    format!(
                        "expected `biomes` to be an array of strings, found {}",
                        biomes_value.type_str()
                    ),
                );
                return None;
            };
            if biome_rows.is_empty() {
                err(
                    diagnostics,
                    entry,
                    "`biomes` must name at least one biome".to_string(),
                );
                return None;
            }
            let mut biomes = Vec::with_capacity(biome_rows.len());
            for (biome_index, biome) in biome_rows.iter().enumerate() {
                let Some(biome) = biome.as_str() else {
                    err(
                        diagnostics,
                        entry,
                        format!(
                            "expected `biomes[{biome_index}]` to be a string, found {}",
                            biome.type_str()
                        ),
                    );
                    return None;
                };
                if names::biome_from_name(biome).is_none() {
                    err(
                        diagnostics,
                        entry,
                        format!(
                            "unknown biome `{biome}`; expected one of {}",
                            names::BIOME_NAMES.join(", ")
                        ),
                    );
                    return None;
                }
                biomes.push(biome.to_string());
            }
            let chance =
                required_number_in(table, "chance", 0.0, 1.0, &entry, diagnostics).ok()? as f32;
            WorldgenKind::Patch {
                block,
                replace,
                biomes,
                min_y,
                max_y,
                chance,
            }
        }
        other => {
            err(
                diagnostics,
                entry,
                format!("unknown worldgen `type` `{other}`"),
            );
            return None;
        }
    };

    Some(WorldgenEntry { kind })
}

fn parse_block_patch(
    value: &toml::Value,
    index: usize,
    patch_ids: &mut HashSet<String>,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Option<BlockPatch> {
    let Some(table) = value.as_table() else {
        err(
            diagnostics,
            format!("block_patch #{}", index + 1),
            format!("expected a table, found {}", value.type_str()),
        );
        return None;
    };
    let entry = entry_label("block_patch", table, index);
    if !within_cap(index, "block_patches", &entry, diagnostics) {
        return None;
    }

    let id = required_string(table, "id", &entry, diagnostics).ok()?;
    if names::builtin_block_id_by_name(&id).is_none() {
        err(diagnostics, entry, format!("unknown built-in block `{id}`"));
        return None;
    }
    let has_field = ["name", "hardness", "drop", "tool", "stack"]
        .iter()
        .any(|key| table.contains_key(*key));
    if !has_field {
        err(
            diagnostics,
            entry,
            "block patch must set at least one of `name`, `hardness`, `drop`, `tool`, `stack`",
        );
        return None;
    }

    let display_name = optional_string(table, "name", &entry, diagnostics).ok()?;
    let hardness = match optional_number(table, "hardness", &entry, diagnostics).ok()? {
        Some(hardness) => {
            if !(0.0..=3600.0).contains(&hardness) {
                err(
                    diagnostics,
                    entry,
                    format!("`hardness` must be in 0..=3600, found {hardness}"),
                );
                return None;
            }
            Some(hardness as f32)
        }
        None => None,
    };
    let drop = optional_string(table, "drop", &entry, diagnostics).ok()?;
    let tool = parse_block_tool(table, &entry, diagnostics).ok()?;
    let stack_limit = match optional_integer(table, "stack", &entry, diagnostics).ok()? {
        Some(stack) => {
            if !(1..=999).contains(&stack) {
                err(
                    diagnostics,
                    entry,
                    format!("`stack` must be in 1..=999, found {stack}"),
                );
                return None;
            }
            Some(stack as u16)
        }
        None => None,
    };

    if !patch_ids.insert(id.clone()) {
        err(
            diagnostics,
            entry,
            format!("duplicate block patch `{id}`; the first definition wins"),
        );
        return None;
    }

    Some(BlockPatch {
        id,
        display_name,
        hardness,
        drop,
        tool,
        stack_limit,
    })
}

fn parse_recipe_patch(
    value: &toml::Value,
    index: usize,
    patch_ids: &mut HashSet<String>,
    diagnostics: &mut Vec<ContentDiagnostic>,
) -> Option<RecipePatch> {
    let Some(table) = value.as_table() else {
        err(
            diagnostics,
            format!("recipe_patch #{}", index + 1),
            format!("expected a table, found {}", value.type_str()),
        );
        return None;
    };
    let entry = entry_label("recipe_patch", table, index);
    if !within_cap(index, "recipe_patches", &entry, diagnostics) {
        return None;
    }

    let id = required_string(table, "id", &entry, diagnostics).ok()?;
    if names::builtin_recipe_id_by_name(&id).is_none() {
        err(
            diagnostics,
            entry,
            format!("unknown built-in recipe `{id}`"),
        );
        return None;
    }
    let enabled = required_bool(table, "enabled", &entry, diagnostics).ok()?;
    if !patch_ids.insert(id.clone()) {
        err(
            diagnostics,
            entry,
            format!("duplicate recipe patch `{id}`; the first definition wins"),
        );
        return None;
    }

    Some(RecipePatch { id, enabled })
}

// ---------------------------------------------------------------------------
// Cross-reference validation
// ---------------------------------------------------------------------------

fn is_item_ref(reference: &str, content_ids: &HashSet<&str>) -> bool {
    content_ids.contains(reference) || names::builtin_item_id_by_name(reference).is_some()
}

fn is_block_ref(reference: &str, block_ids: &HashSet<&str>) -> bool {
    block_ids.contains(reference) || names::builtin_block_id_by_name(reference).is_some()
}

/// Reject entries whose references do not resolve inside the built-ins plus
/// this pack. Cross-pack references are unsupported in M010, so a typo in an
/// ingredient name fails here instead of silently producing an unusable
/// definition.
fn validate_references(spec: &mut PackContentSpec, diagnostics: &mut Vec<ContentDiagnostic>) {
    let content_ids: HashSet<&str> = spec
        .blocks
        .iter()
        .map(|entry| entry.id.as_str())
        .chain(spec.items.iter().map(|entry| entry.id.as_str()))
        .chain(spec.tools.iter().map(|entry| entry.id.as_str()))
        .collect();
    let block_ids: HashSet<&str> = spec.blocks.iter().map(|entry| entry.id.as_str()).collect();
    let tier_ids: HashSet<&str> = spec.tiers.iter().map(|entry| entry.id.as_str()).collect();

    let mut bad_blocks: HashSet<String> = HashSet::new();
    for block in &spec.blocks {
        if let Some(drop) = &block.drop {
            if !is_item_ref(drop, &content_ids) {
                err(
                    diagnostics,
                    format!("block {}", block.id),
                    format!(
                        "unknown drop `{drop}`; references must name a built-in item or an entry in this pack (cross-pack references are not supported in M010)"
                    ),
                );
                bad_blocks.insert(block.id.clone());
            }
        }
    }

    let mut bad_recipes: HashSet<String> = HashSet::new();
    for recipe in &spec.recipes {
        let mut bad = false;
        for (character, item) in &recipe.key {
            if !is_item_ref(item, &content_ids) {
                err(
                    diagnostics,
                    format!("recipe {}", recipe.id),
                    format!(
                        "unknown ingredient `{item}` for key `{character}`; references must name a built-in item or an entry in this pack (cross-pack references are not supported in M010)"
                    ),
                );
                bad = true;
            }
        }
        if !is_item_ref(&recipe.output, &content_ids) {
            err(
                diagnostics,
                format!("recipe {}", recipe.id),
                format!(
                    "unknown output `{}`; references must name a built-in item or an entry in this pack (cross-pack references are not supported in M010)",
                    recipe.output
                ),
            );
            bad = true;
        }
        for row in &recipe.pattern {
            for character in row.chars() {
                if character != ' ' && !recipe.key.contains_key(&character) {
                    err(
                        diagnostics,
                        format!("recipe {}", recipe.id),
                        format!("pattern character `{character}` has no `key` entry"),
                    );
                    bad = true;
                }
            }
        }
        if bad {
            bad_recipes.insert(recipe.id.clone());
        }
    }

    let mut bad_tools: HashSet<String> = HashSet::new();
    for tool in &spec.tools {
        if names::builtin_tier_by_name(&tool.tier).is_none()
            && !tier_ids.contains(tool.tier.as_str())
        {
            err(
                diagnostics,
                format!("tool {}", tool.id),
                format!(
                    "unknown tier `{}`; references must name a built-in tier or a tier in this pack (cross-pack references are not supported in M010)",
                    tool.tier
                ),
            );
            bad_tools.insert(tool.id.clone());
        }
    }

    let mut bad_block_patches: HashSet<String> = HashSet::new();
    for patch in &spec.block_patches {
        if let Some(drop) = &patch.drop {
            if !is_item_ref(drop, &content_ids) {
                err(
                    diagnostics,
                    format!("block_patch {}", patch.id),
                    format!(
                        "unknown drop `{drop}`; references must name a built-in item or an entry in this pack (cross-pack references are not supported in M010)"
                    ),
                );
                bad_block_patches.insert(patch.id.clone());
            }
        }
    }

    let mut worldgen_keep = Vec::with_capacity(spec.worldgen.len());
    for entry in std::mem::take(&mut spec.worldgen) {
        let (block, replace) = match &entry.kind {
            WorldgenKind::Ore { block, replace, .. }
            | WorldgenKind::Patch { block, replace, .. } => (block, replace),
        };
        let label = match &entry.kind {
            WorldgenKind::Ore { block, .. } => format!("worldgen ore {block}"),
            WorldgenKind::Patch { block, .. } => format!("worldgen patch {block}"),
        };

        let mut bad = false;
        for (field, reference) in [("block", block), ("replace", replace)] {
            if !is_block_ref(reference, &block_ids) {
                err(
                    diagnostics,
                    label.clone(),
                    format!(
                        "unknown {field} reference `{reference}`; references must name a built-in block or a block in this pack (cross-pack references are not supported in M010)"
                    ),
                );
                bad = true;
            }
        }
        if !bad {
            worldgen_keep.push(entry);
        }
    }
    spec.worldgen = worldgen_keep;

    spec.blocks.retain(|entry| !bad_blocks.contains(&entry.id));
    spec.recipes
        .retain(|entry| !bad_recipes.contains(&entry.id));
    spec.tools.retain(|entry| !bad_tools.contains(&entry.id));
    spec.block_patches
        .retain(|entry| !bad_block_patches.contains(&entry.id));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    use crate::content::spec::WorldgenKind;

    const VALID_PACK: &str = r#"
format_version = 2
name = "Ruby Mod"
version = "1.0.0"

[[blocks]]
id = "mymod:ruby_ore"
name = "Ruby Ore"
textures = { all = "ruby_ore.png" }
hardness = 3.0
drop = "mymod:ruby"
tool = "pickaxe"
stack = 64

[[blocks]]
id = "mymod:ruby_block"
textures = { top = "ruby_block_top.png", bottom = "ruby_block_bottom.png", side = "ruby_block_side.png" }
hardness = 4

[[items]]
id = "mymod:ruby"
icon = "ruby.png"
stack = 64
nutrition = 4
eat_seconds = 0.8

[[tiers]]
id = "mymod:ruby"
speed = 9.0
durability = 900
damage = 6

[[tools]]
id = "mymod:ruby_pickaxe"
kind = "pickaxe"
tier = "mymod:ruby"
icon = "ruby_pickaxe.png"

[[recipes]]
id = "mymod:ruby_pickaxe"
pattern = ["RRR", " S ", " S "]
key = { R = "mymod:ruby", S = "stick" }
output = "mymod:ruby_pickaxe"
count = 1

[[worldgen]]
type = "ore"
block = "mymod:ruby_ore"
replace = "stone"
min_y = -48
max_y = 16
vein = 6
attempts = 12

[[worldgen]]
type = "patch"
block = "mymod:ruby_block"
replace = "grass"
biomes = ["plains", "forest"]
min_y = 60
max_y = 80
chance = 0.01

[[block_patches]]
id = "stone"
name = "Granite"
hardness = 3.5
drop = "stone"

[[recipe_patches]]
id = "planks"
enabled = false
"#;

    fn parse_v2(
        text: &str,
    ) -> (
        PackContentSpec,
        Vec<SettingsWarning>,
        Vec<ContentDiagnostic>,
    ) {
        let root: toml::Value = toml::from_str(text).expect("fixture TOML must parse");
        parse_content(&root, PackFormat::V2)
    }

    fn parse_format(
        text: &str,
        format: PackFormat,
    ) -> (
        PackContentSpec,
        Vec<SettingsWarning>,
        Vec<ContentDiagnostic>,
    ) {
        let root: toml::Value = toml::from_str(text).expect("fixture TOML must parse");
        parse_content(&root, format)
    }

    fn entries(diagnostics: &[ContentDiagnostic]) -> Vec<&str> {
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.entry.as_str())
            .collect()
    }

    fn reasons(diagnostics: &[ContentDiagnostic]) -> Vec<&str> {
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.reason.as_str())
            .collect()
    }

    fn assert_has_reason(diagnostics: &[ContentDiagnostic], needle: &str) {
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.reason.contains(needle)),
            "expected a diagnostic containing `{needle}`: {:?}",
            reasons(diagnostics)
        );
    }

    fn pack_dir(root: &tempfile::TempDir) -> PathBuf {
        let dir = root.path().join("pack");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn gradient(width: u32, height: u32) -> Vec<u8> {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                data.extend_from_slice(&[
                    (x.wrapping_mul(7).wrapping_add(3)) as u8,
                    (y.wrapping_mul(11).wrapping_add(5)) as u8,
                    ((x + y).wrapping_mul(3)) as u8,
                    255,
                ]);
            }
        }
        data
    }

    fn write_png_data(path: &std::path::Path, width: u32, height: u32, data: &[u8]) {
        let file = fs::File::create(path).expect("create fixture png");
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("write png header");
        writer.write_image_data(data).expect("write png image data");
    }

    fn write_png(path: &std::path::Path, dim: u32) {
        write_png_data(path, dim, dim, &gradient(dim, dim));
    }

    fn load_assets(text: &str, dir: &std::path::Path) -> (PackContentSpec, Vec<ContentDiagnostic>) {
        let (mut spec, warnings, mut diagnostics) = parse_v2(text);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        load_content_assets(dir, &mut spec, &mut diagnostics);
        (spec, diagnostics)
    }

    #[test]
    fn valid_full_pack_parses_without_diagnostics() {
        let (spec, warnings, diagnostics) = parse_v2(VALID_PACK);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(
            diagnostics.is_empty(),
            "unexpected diagnostics: {diagnostics:?}"
        );

        assert_eq!(spec.blocks.len(), 2);
        let ore = &spec.blocks[0];
        assert_eq!(ore.id, "mymod:ruby_ore");
        assert_eq!(ore.display_name.as_deref(), Some("Ruby Ore"));
        assert_eq!(ore.textures.all.as_deref(), Some("ruby_ore.png"));
        assert_eq!(ore.hardness, 3.0);
        assert_eq!(ore.drop.as_deref(), Some("mymod:ruby"));
        assert_eq!(ore.tool, Some(ToolKind::Pickaxe));
        assert_eq!(ore.stack_limit, 64);

        let block = &spec.blocks[1];
        assert_eq!(block.textures.top.as_deref(), Some("ruby_block_top.png"));
        assert_eq!(
            block.textures.bottom.as_deref(),
            Some("ruby_block_bottom.png")
        );
        assert_eq!(block.textures.side.as_deref(), Some("ruby_block_side.png"));
        assert_eq!(block.hardness, 4.0);
        assert_eq!(block.stack_limit, 999, "block stack defaults to 999");

        assert_eq!(spec.items.len(), 1);
        let item = &spec.items[0];
        assert_eq!(item.id, "mymod:ruby");
        assert_eq!(item.icon, "ruby.png");
        assert_eq!(item.stack_limit, 64);
        assert_eq!(
            item.food,
            Some(FoodSpec {
                nutrition: 4,
                eat_seconds: 0.8
            })
        );

        assert_eq!(spec.tiers.len(), 1);
        assert_eq!(spec.tiers[0].speed, 9.0);
        assert_eq!(spec.tiers[0].durability, 900);
        assert_eq!(spec.tiers[0].damage, 6);

        assert_eq!(spec.tools.len(), 1);
        assert_eq!(spec.tools[0].kind, ToolKind::Pickaxe);
        assert_eq!(spec.tools[0].tier, "mymod:ruby");

        assert_eq!(spec.recipes.len(), 1);
        let recipe = &spec.recipes[0];
        assert_eq!(recipe.pattern, vec!["RRR", " S ", " S "]);
        assert_eq!(recipe.key.get(&'R').map(String::as_str), Some("mymod:ruby"));
        assert_eq!(recipe.key.get(&'S').map(String::as_str), Some("stick"));
        assert_eq!(recipe.output, "mymod:ruby_pickaxe");
        assert_eq!(recipe.count, 1);

        assert_eq!(spec.worldgen.len(), 2);
        match &spec.worldgen[0].kind {
            WorldgenKind::Ore {
                block,
                replace,
                min_y,
                max_y,
                vein,
                attempts,
            } => {
                assert_eq!(block, "mymod:ruby_ore");
                assert_eq!(replace, "stone");
                assert_eq!((*min_y, *max_y), (-48, 16));
                assert_eq!((*vein, *attempts), (6, 12));
            }
            other => panic!("expected an ore rule, got {other:?}"),
        }
        match &spec.worldgen[1].kind {
            WorldgenKind::Patch { biomes, chance, .. } => {
                assert_eq!(biomes, &vec!["plains".to_string(), "forest".to_string()]);
                assert_eq!(*chance, 0.01);
            }
            other => panic!("expected a patch rule, got {other:?}"),
        }

        assert_eq!(spec.block_patches.len(), 1);
        assert_eq!(spec.block_patches[0].id, "stone");
        assert_eq!(
            spec.block_patches[0].display_name.as_deref(),
            Some("Granite")
        );
        assert_eq!(spec.block_patches[0].hardness, Some(3.5));
        assert_eq!(spec.block_patches[0].drop.as_deref(), Some("stone"));

        assert_eq!(spec.recipe_patches.len(), 1);
        assert_eq!(spec.recipe_patches[0].id, "planks");
        assert!(!spec.recipe_patches[0].enabled);
    }

    #[test]
    fn referenced_assets_load_resampled_and_sorted_once() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);
        for file in [
            "ruby_ore.png",
            "ruby_block_top.png",
            "ruby_block_bottom.png",
            "ruby_block_side.png",
            "ruby.png",
            "ruby_pickaxe.png",
        ] {
            write_png(&dir.join(file), 32);
        }

        let (spec, diagnostics) = load_assets(VALID_PACK, &dir);
        assert!(
            diagnostics.is_empty(),
            "unexpected diagnostics: {diagnostics:?}"
        );
        assert_eq!(spec.assets.len(), 6, "each unique path loads once");
        let keys: Vec<&str> = spec.assets.keys().map(String::as_str).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "asset map stays deterministically sorted");

        let expected = resample::resample_to_16(&gradient(32, 32), 32);
        for asset in spec.assets.values() {
            assert_eq!(asset.source_side, 32);
            assert_eq!(asset.rgba.len(), 16 * 16 * 4);
            assert_eq!(asset.rgba, expected);
        }
    }

    #[test]
    fn duplicate_asset_paths_load_once() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);
        write_png(&dir.join("shared.png"), 16);

        let text = r#"
[[blocks]]
id = "mymod:a"
textures = { all = "shared.png" }
hardness = 1.0

[[blocks]]
id = "mymod:b"
textures = { all = "shared.png" }
hardness = 1.0
"#;
        let (spec, diagnostics) = load_assets(text, &dir);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(spec.assets.len(), 1);
    }

    #[test]
    fn v1_content_arrays_warn_once_and_are_ignored() {
        let text = r#"
[[blocks]]
id = "mymod:ruby"
textures = { all = "ruby.png" }
hardness = 1.0
"#;
        let (spec, warnings, diagnostics) = parse_format(text, PackFormat::V1);
        assert_eq!(spec, PackContentSpec::default());
        assert!(diagnostics.is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].field, "pack.toml");
        assert!(warnings[0]
            .reason
            .contains("content arrays require format_version = 2"));
    }

    #[test]
    fn v1_without_content_arrays_is_silent() {
        let (spec, warnings, diagnostics) = parse_format("name = \"Plain\"\n", PackFormat::V1);
        assert!(spec.blocks.is_empty());
        assert!(warnings.is_empty());
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn future_format_warns_once_and_ignores_content() {
        let (spec, warnings, diagnostics) = parse_format(VALID_PACK, PackFormat::Future(3));
        assert!(spec.blocks.is_empty());
        assert!(diagnostics.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].reason.contains("newer than 2"));
        assert!(warnings[0].reason.contains("ignored"));
    }

    #[test]
    fn missing_id_is_rejected_but_sibling_loads() {
        let text = r#"
[[blocks]]
textures = { all = "a.png" }
hardness = 1.0

[[blocks]]
id = "mymod:valid"
textures = { all = "b.png" }
hardness = 1.0
"#;
        let (spec, _, diagnostics) = parse_v2(text);
        assert_eq!(spec.blocks.len(), 1);
        assert_eq!(spec.blocks[0].id, "mymod:valid");
        assert_has_reason(&diagnostics, "missing required field `id`");
    }

    #[test]
    fn duplicate_id_first_wins() {
        let text = r#"
[[items]]
id = "mymod:ruby"
icon = "ruby.png"

[[items]]
id = "mymod:ruby"
icon = "ruby2.png"
"#;
        let (spec, _, diagnostics) = parse_v2(text);
        assert_eq!(spec.items.len(), 1);
        assert_eq!(spec.items[0].icon, "ruby.png");
        assert_has_reason(&diagnostics, "duplicate content id");
        assert_eq!(entries(&diagnostics), vec!["item mymod:ruby"]);
    }

    #[test]
    fn malformed_and_reserved_ids_are_rejected() {
        let text = r#"
[[blocks]]
id = "opencraft:ruby"
textures = { all = "a.png" }
hardness = 1.0

[[blocks]]
id = "not_namespaced"
textures = { all = "a.png" }
hardness = 1.0

[[blocks]]
id = "MyMod:Ruby"
textures = { all = "a.png" }
hardness = 1.0

[[blocks]]
id = "mymod:valid"
textures = { all = "a.png" }
hardness = 1.0
"#;
        let (spec, _, diagnostics) = parse_v2(text);
        assert_eq!(spec.blocks.len(), 1);
        assert_eq!(spec.blocks[0].id, "mymod:valid");
        assert_eq!(diagnostics.len(), 3);
        assert_has_reason(&diagnostics, "reserved");
        assert_has_reason(&diagnostics, "not a namespaced id");
    }

    #[test]
    fn over_long_ids_are_rejected_before_any_save_can_fail() {
        let long_id = format!("mymod:{}", "a".repeat(names::MAX_CONTENT_NAME_LEN));
        assert!(long_id.len() > names::MAX_CONTENT_NAME_LEN);
        let text = format!(
            r#"
[[items]]
id = "{long_id}"
icon = "a.png"

[[items]]
id = "mymod:valid"
icon = "b.png"
"#
        );
        let (spec, _, diagnostics) = parse_v2(&text);
        assert_eq!(spec.items.len(), 1);
        assert_eq!(spec.items[0].id, "mymod:valid");
        assert_has_reason(&diagnostics, "content name limit");
        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn blocks_items_and_tools_share_one_content_namespace() {
        let text = r#"
[[blocks]]
id = "mymod:ruby"
textures = { all = "a.png" }
hardness = 1.0

[[items]]
id = "mymod:ruby"
icon = "ruby.png"

[[tools]]
id = "mymod:ruby"
kind = "pickaxe"
tier = "wood"
icon = "pick.png"
"#;
        let (spec, _, diagnostics) = parse_v2(text);
        assert_eq!(spec.blocks.len(), 1);
        assert!(spec.items.is_empty());
        assert!(spec.tools.is_empty());
        assert_eq!(
            entries(&diagnostics),
            vec!["item mymod:ruby", "tool mymod:ruby"]
        );
        assert_has_reason(&diagnostics, "duplicate content id");
    }

    #[test]
    fn tiers_and_recipes_have_independent_namespaces() {
        let text = r#"
[[tiers]]
id = "mymod:ruby"
speed = 5.0
durability = 100
damage = 4

[[recipes]]
id = "mymod:ruby"
pattern = ["R"]
key = { R = "stick" }
output = "stick"
count = 1
"#;
        let (spec, _, diagnostics) = parse_v2(text);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(spec.tiers.len(), 1);
        assert_eq!(spec.recipes.len(), 1);
    }

    #[test]
    fn unknown_drop_rejects_only_that_block() {
        let text = r#"
[[blocks]]
id = "mymod:bad"
textures = { all = "a.png" }
hardness = 1.0
drop = "mymod:ghost"

[[blocks]]
id = "mymod:good"
textures = { all = "a.png" }
hardness = 1.0
drop = "stone"
"#;
        let (spec, _, diagnostics) = parse_v2(text);
        assert_eq!(spec.blocks.len(), 1);
        assert_eq!(spec.blocks[0].id, "mymod:good");
        assert_eq!(entries(&diagnostics), vec!["block mymod:bad"]);
        assert_has_reason(&diagnostics, "unknown drop");
        assert_has_reason(&diagnostics, "cross-pack references are not supported");
    }

    #[test]
    fn unknown_recipe_ingredient_and_output_are_rejected() {
        let text = r#"
[[recipes]]
id = "mymod:bad"
pattern = ["RR"]
key = { R = "mymod:dust" }
output = "mymod:missing"
count = 1
"#;
        let (spec, _, diagnostics) = parse_v2(text);
        assert!(spec.recipes.is_empty());
        assert_has_reason(&diagnostics, "unknown ingredient `mymod:dust`");
        assert_has_reason(&diagnostics, "unknown output `mymod:missing`");
    }

    #[test]
    fn recipe_pattern_characters_must_have_keys() {
        let text = r#"
[[recipes]]
id = "mymod:bad"
pattern = ["RRR"]
key = { S = "stick" }
output = "stick"
count = 1
"#;
        let (spec, _, diagnostics) = parse_v2(text);
        assert!(spec.recipes.is_empty());
        assert_has_reason(&diagnostics, "pattern character `R` has no `key` entry");
    }

    #[test]
    fn recipe_pattern_and_count_bounds_are_enforced() {
        for (pattern, count) in [
            ("[]", "1"),
            ("[\"RRRRR\"]", "1"),
            ("[\"\", \"R\"]", "1"),
            ("[\"R\"]", "0"),
            ("[\"R\"]", "1000"),
        ] {
            let text = format!(
                "[[recipes]]\nid = \"mymod:r\"\npattern = {pattern}\nkey = {{ R = \"stick\" }}\noutput = \"stick\"\ncount = {count}\n"
            );
            let (spec, _, diagnostics) = parse_v2(&text);
            assert!(spec.recipes.is_empty(), "pattern {pattern} count {count}");
            assert!(
                !diagnostics.is_empty(),
                "pattern {pattern} count {count} should be rejected"
            );
        }
    }

    #[test]
    fn unknown_tool_tier_is_rejected() {
        let text = r#"
[[tools]]
id = "mymod:ruby_pickaxe"
kind = "pickaxe"
tier = "mymod:ghost"
icon = "pick.png"
"#;
        let (spec, _, diagnostics) = parse_v2(text);
        assert!(spec.tools.is_empty());
        assert_has_reason(&diagnostics, "unknown tier `mymod:ghost`");
    }

    #[test]
    fn bow_tools_are_rejected() {
        let text = r#"
[[tools]]
id = "mymod:bow"
kind = "bow"
tier = "wood"
icon = "bow.png"
"#;
        let (spec, _, diagnostics) = parse_v2(text);
        assert!(spec.tools.is_empty());
        assert_has_reason(&diagnostics, "not authorable");
    }

    #[test]
    fn unknown_worldgen_blocks_are_rejected() {
        let text = r#"
[[worldgen]]
type = "ore"
block = "mymod:ghost"
replace = "stone"
min_y = 0
max_y = 16
vein = 4
attempts = 4

[[worldgen]]
type = "patch"
block = "stone"
replace = "mymod:ghost"
biomes = ["plains"]
min_y = 0
max_y = 16
chance = 0.1
"#;
        let (spec, _, diagnostics) = parse_v2(text);
        assert!(spec.worldgen.is_empty());
        assert_has_reason(&diagnostics, "unknown block reference `mymod:ghost`");
        assert_has_reason(&diagnostics, "unknown replace reference `mymod:ghost`");
    }

    #[test]
    fn non_finite_and_out_of_range_hardness_is_rejected() {
        for hardness in ["nan", "inf", "-1", "3601"] {
            let text = format!(
                "[[blocks]]\nid = \"mymod:r\"\ntextures = {{ all = \"a.png\" }}\nhardness = {hardness}\n"
            );
            let (spec, _, diagnostics) = parse_v2(&text);
            assert!(spec.blocks.is_empty(), "hardness {hardness}");
            assert!(!diagnostics.is_empty(), "hardness {hardness} should fail");
        }
    }

    #[test]
    fn worldgen_caps_reject_bad_rules() {
        let text = r#"
[[worldgen]]
type = "ore"
block = "stone"
replace = "dirt"
min_y = 0
max_y = 16
vein = 9
attempts = 4

[[worldgen]]
type = "ore"
block = "stone"
replace = "dirt"
min_y = 0
max_y = 16
vein = 4
attempts = 33

[[worldgen]]
type = "ore"
block = "stone"
replace = "dirt"
min_y = 0
max_y = 200
vein = 4
attempts = 4

[[worldgen]]
type = "patch"
block = "stone"
replace = "dirt"
biomes = []
min_y = 0
max_y = 16
chance = 0.1

[[worldgen]]
type = "patch"
block = "stone"
replace = "dirt"
biomes = ["desert"]
min_y = 0
max_y = 16
chance = 0.1

[[worldgen]]
type = "patch"
block = "stone"
replace = "dirt"
biomes = ["plains"]
min_y = 0
max_y = 16
chance = 2.0
"#;
        let (spec, _, diagnostics) = parse_v2(text);
        assert!(spec.worldgen.is_empty());
        assert_eq!(diagnostics.len(), 6, "{diagnostics:?}");
        assert_has_reason(&diagnostics, "`vein` must be in 1..=8");
        assert_has_reason(&diagnostics, "`attempts` must be in 1..=32");
        assert_has_reason(&diagnostics, "`max_y` must be in -64..=128");
        assert_has_reason(&diagnostics, "at least one biome");
        assert_has_reason(&diagnostics, "unknown biome `desert`");
        assert_has_reason(&diagnostics, "`chance` must be in 0..=1");
    }

    #[test]
    fn worldgen_requires_min_below_max() {
        let text = r#"
[[worldgen]]
type = "ore"
block = "stone"
replace = "dirt"
min_y = 16
max_y = 16
vein = 4
attempts = 4
"#;
        let (spec, _, diagnostics) = parse_v2(text);
        assert!(spec.worldgen.is_empty());
        assert_has_reason(&diagnostics, "must be less than");
    }

    #[test]
    fn block_and_recipe_patches_validate_their_targets_and_fields() {
        let text = r#"
[[block_patches]]
id = "mymod:stone"
hardness = 1.0

[[block_patches]]
id = "stone"

[[recipe_patches]]
id = "mymod:recipe"
enabled = false

[[recipe_patches]]
id = "planks"
enabled = false

[[block_patches]]
id = "sand"
tool = "sword"

[[block_patches]]
id = "sand"
hardness = 2.0
"#;
        let (spec, _, diagnostics) = parse_v2(text);
        assert_eq!(spec.block_patches.len(), 1);
        assert_eq!(spec.block_patches[0].id, "sand");
        assert_eq!(spec.block_patches[0].hardness, Some(2.0));
        assert_eq!(spec.recipe_patches.len(), 1);
        assert_eq!(spec.recipe_patches[0].id, "planks");
        assert_has_reason(&diagnostics, "unknown built-in block `mymod:stone`");
        assert_has_reason(&diagnostics, "must set at least one");
        assert_has_reason(&diagnostics, "unknown built-in recipe `mymod:recipe`");
        assert_has_reason(&diagnostics, "not a block accelerator");
    }

    #[test]
    fn item_food_defaults_and_bounds() {
        let text = r#"
[[items]]
id = "mymod:food"
icon = "food.png"
nutrition = 4
"#;
        let (spec, _, diagnostics) = parse_v2(text);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(
            spec.items[0].food,
            Some(FoodSpec {
                nutrition: 4,
                eat_seconds: 1.2
            })
        );

        for nutrition in ["0", "21"] {
            let text = format!(
                "[[items]]\nid = \"mymod:food\"\nicon = \"food.png\"\nnutrition = {nutrition}\n"
            );
            let (spec, _, diagnostics) = parse_v2(&text);
            assert!(spec.items.is_empty(), "nutrition {nutrition}");
            assert!(!diagnostics.is_empty());
        }
    }

    #[test]
    fn asset_path_escapes_are_rejected_at_parse_time() {
        let text = r#"
[[blocks]]
id = "mymod:absolute"
textures = { all = "/etc/passwd" }
hardness = 1.0

[[items]]
id = "mymod:parent"
icon = "../outside.png"

[[tools]]
id = "mymod:tool"
kind = "pickaxe"
tier = "wood"
icon = "./inside.png"
"#;
        let (spec, _, diagnostics) = parse_v2(text);
        assert!(spec.blocks.is_empty());
        assert!(spec.items.is_empty());
        assert!(spec.tools.is_empty());
        assert_eq!(diagnostics.len(), 3);
        assert_has_reason(&diagnostics, "must be relative");
        assert_has_reason(&diagnostics, "must not contain `..`");
        assert_has_reason(&diagnostics, "must not contain `.`");
    }

    #[test]
    fn invalid_asset_images_are_diagnosed_and_skipped() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);
        write_png(&dir.join("small.png"), 8);
        write_png(&dir.join("big.png"), 512);
        write_png_data(&dir.join("wide.png"), 32, 16, &gradient(32, 16));
        write_png(&dir.join("good.png"), 32);

        let text = r#"
[[blocks]]
id = "mymod:small"
textures = { all = "small.png" }
hardness = 1.0

[[blocks]]
id = "mymod:big"
textures = { all = "big.png" }
hardness = 1.0

[[blocks]]
id = "mymod:wide"
textures = { all = "wide.png" }
hardness = 1.0

[[blocks]]
id = "mymod:good"
textures = { all = "good.png" }
hardness = 1.0
"#;
        let (spec, diagnostics) = load_assets(text, &dir);
        assert_eq!(spec.assets.len(), 1, "only the valid asset is stored");
        assert!(spec.assets.contains_key("good.png"));
        assert_eq!(diagnostics.len(), 3, "{diagnostics:?}");
        assert_has_reason(&diagnostics, "between 16 and 256");
        assert_has_reason(&diagnostics, "must be square");
    }

    #[test]
    fn asset_diagnostics_are_sorted_and_missing_files_stay_per_path() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);

        let text = r#"
[[blocks]]
id = "mymod:z"
textures = { all = "z.png" }
hardness = 1.0

[[blocks]]
id = "mymod:a"
textures = { all = "a.png" }
hardness = 1.0
"#;
        let (spec, diagnostics) = load_assets(text, &dir);
        assert!(spec.assets.is_empty());
        assert_eq!(
            entries(&diagnostics),
            vec!["asset a.png", "asset z.png"],
            "assets load in sorted path order, not entry order"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_asset_escape_is_diagnosed() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);
        let outside = root.path().join("outside.png");
        write_png(&outside, 16);
        symlink(&outside, dir.join("escape.png")).unwrap();

        let text = r#"
[[blocks]]
id = "mymod:escape"
textures = { all = "escape.png" }
hardness = 1.0
"#;
        let (spec, diagnostics) = load_assets(text, &dir);
        assert!(spec.assets.is_empty());
        assert_eq!(entries(&diagnostics), vec!["asset escape.png"]);
        assert_has_reason(&diagnostics, "escapes the pack folder");
    }

    #[test]
    fn per_file_asset_cap_is_diagnosed() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);
        fs::write(
            dir.join("huge.bin"),
            vec![0u8; asset::PACK_ASSET_BYTE_CAP as usize + 1],
        )
        .unwrap();

        let text = r#"
[[items]]
id = "mymod:huge"
icon = "huge.bin"
"#;
        let (spec, diagnostics) = load_assets(text, &dir);
        assert!(spec.assets.is_empty());
        assert_has_reason(&diagnostics, "per-file limit");
    }

    #[test]
    fn oversized_ihdr_asset_is_diagnosed() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);
        fs::write(dir.join("huge.png"), decode::png_bomb_bytes(30000, 30000)).unwrap();

        let text = r#"
[[blocks]]
id = "mymod:huge"
textures = { all = "huge.png" }
hardness = 1.0
"#;
        let (spec, diagnostics) = load_assets(text, &dir);
        assert!(spec.assets.is_empty());
        assert_eq!(entries(&diagnostics), vec!["asset huge.png"]);
        assert!(
            diagnostics[0].reason.contains("30000x30000"),
            "diagnostic should report the IHDR size: {diagnostics:?}"
        );
        assert!(
            diagnostics[0].reason.contains("256"),
            "diagnostic should mention the limit: {diagnostics:?}"
        );
    }

    #[test]
    fn total_asset_cap_stops_further_loading() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);
        for index in 0..9 {
            fs::write(dir.join(format!("a{index}.bin")), vec![0xAB; 1_000_000]).unwrap();
        }

        let mut text = String::new();
        for index in 0..9 {
            text.push_str(&format!(
                "[[blocks]]\nid = \"mymod:b{index}\"\ntextures = {{ all = \"a{index}.bin\" }}\nhardness = 1.0\n\n"
            ));
        }

        let (spec, diagnostics) = load_assets(&text, &dir);
        assert!(spec.assets.is_empty(), "no non-PNG asset can be stored");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.entry == "pack" && diagnostic.reason.contains("total limit")
            }),
            "expected one pack-level total-cap error: {diagnostics:?}"
        );
    }

    #[test]
    fn builtin_icon_table_accepts_materials_and_tools() {
        let text = r#"
[builtin_icons]
apple = "textures/apple.png"
wood_pickaxe = "textures/pickaxe.png"
wood_bow = "textures/bow.png"
"#;
        let (spec, warnings, diagnostics) = parse_v2(text);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(
            diagnostics.is_empty(),
            "unexpected diagnostics: {diagnostics:?}"
        );
        assert_eq!(spec.builtin_icons.len(), 3);
        assert_eq!(spec.builtin_icons["apple"], "textures/apple.png");
        assert_eq!(spec.builtin_icons["wood_pickaxe"], "textures/pickaxe.png");
        assert_eq!(spec.builtin_icons["wood_bow"], "textures/bow.png");
        let names: Vec<&str> = spec.builtin_icons.keys().map(String::as_str).collect();
        assert_eq!(names, ["apple", "wood_bow", "wood_pickaxe"]);
    }

    #[test]
    fn builtin_icon_table_must_be_a_table() {
        let (spec, _, diagnostics) = parse_v2("builtin_icons = 3\n");
        assert!(spec.builtin_icons.is_empty());
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].severity, Severity::Error);
        assert_eq!(diagnostics[0].entry, "builtin_icons");
        assert_has_reason(&diagnostics, "expected `[builtin_icons]` to be a table");
    }

    #[test]
    fn builtin_icon_table_over_cap_is_one_error() {
        let mut text = String::from("[builtin_icons]\n");
        for index in 0..=MAX_ARRAY_ENTRIES {
            text.push_str(&format!("key_{index} = \"p{index}.png\"\n"));
        }
        let (spec, _, diagnostics) = parse_v2(&text);
        assert!(spec.builtin_icons.is_empty());
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].entry, "builtin_icons");
        assert_has_reason(&diagnostics, "256-entry limit");
    }

    #[test]
    fn builtin_icon_table_rejects_blocks_unknown_names_and_bad_values() {
        let text = r#"
[builtin_icons]
stone = "stone.png"
diamond = "diamond.png"
apple = 3
wood_axe = "../escaped.png"
"#;
        let (spec, warnings, diagnostics) = parse_v2(text);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(spec.builtin_icons.is_empty());
        assert_eq!(
            entries(&diagnostics),
            vec![
                "builtin_icon apple",
                "builtin_icon diamond",
                "builtin_icon stone",
                "builtin_icon wood_axe",
            ]
        );
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.severity == Severity::Error),
            "every rejection is an error: {diagnostics:?}"
        );
        assert_has_reason(&diagnostics, "is a block; builtin icon overrides target");
        assert_has_reason(&diagnostics, "not a builtin material or tool");
        assert_has_reason(&diagnostics, "expected `path` to be a string");
        assert_has_reason(&diagnostics, "must not contain `..` components");
    }

    #[test]
    fn v1_builtin_icon_table_warns_and_is_ignored() {
        let text = "[builtin_icons]\napple = \"apple.png\"\n";
        let (spec, warnings, diagnostics) = parse_format(text, PackFormat::V1);
        assert_eq!(spec, PackContentSpec::default());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].field, "pack.toml");
        assert_eq!(
            warnings[0].reason,
            "builtin icon overrides require format_version = 2"
        );
    }

    #[test]
    fn v1_content_arrays_win_over_the_builtin_icon_warning() {
        let text = r#"
[[blocks]]
id = "mymod:ruby"
textures = { all = "ruby.png" }
hardness = 1.0

[builtin_icons]
apple = "apple.png"
"#;
        let (spec, warnings, diagnostics) = parse_format(text, PackFormat::V1);
        assert_eq!(spec, PackContentSpec::default());
        assert!(diagnostics.is_empty());
        assert_eq!(
            warnings.len(),
            1,
            "only the array warning fires: {warnings:?}"
        );
        assert_eq!(
            warnings[0].reason,
            "content arrays require format_version = 2"
        );
    }

    #[test]
    fn future_format_ignores_builtin_icons_with_the_existing_warning() {
        let text = "[builtin_icons]\napple = \"apple.png\"\n";
        let (spec, warnings, diagnostics) = parse_format(text, PackFormat::Future(3));
        assert_eq!(spec, PackContentSpec::default());
        assert!(diagnostics.is_empty());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].reason.contains("newer than 2"));
    }

    #[test]
    fn builtin_icon_assets_load_through_the_v2_asset_pipeline() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);
        write_png(&dir.join("apple.png"), 32);

        let text = "[builtin_icons]\napple = \"apple.png\"\n";
        let (spec, diagnostics) = load_assets(text, &dir);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(spec.builtin_icons["apple"], "apple.png");
        let asset = spec
            .assets
            .get("apple.png")
            .expect("override PNG loads like any other v2 asset");
        assert_eq!(asset.source_side, 32);
        assert_eq!(asset.rgba, resample::resample_to_16(&gradient(32, 32), 32));
    }

    #[test]
    fn missing_builtin_icon_asset_uses_the_normal_asset_error() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(&root);

        let text = "[builtin_icons]\napple = \"missing.png\"\n";
        let (spec, diagnostics) = load_assets(text, &dir);
        assert_eq!(spec.builtin_icons["apple"], "missing.png");
        assert!(spec.assets.is_empty());
        assert_eq!(entries(&diagnostics), vec!["asset missing.png"]);
        assert_eq!(diagnostics[0].severity, Severity::Error);
        assert_has_reason(&diagnostics, "missing.png");
    }
}
