//! Headless `--demo-*` digests; every line is machine-independent and pinned
//! whole-line by `scripts/*_smoke.sh`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use glam::{IVec3, Vec3};

use crate::commands::{
    apply_inventory_click_button_with, break_mined_block, save_current_world, BreakOutcome,
    InventoryClickButton, InventoryClickOutcome, ToolWearOutcome,
};
use crate::content::ContentRegistry;
use crate::content::{ItemRef, WorldgenRule};
use crate::player::eating::{EatingState, EatingTarget, EatingTick};
use crate::player::inventory::{
    CraftingGrid, GridKind, Inventory, Item, ItemId, Material, ToolKind, ToolTier,
};
use crate::player::mining::{MiningState, MiningTarget, MiningTick};
use crate::player::physics::Player;
use crate::player::survival::{GameMode, SurvivalState, FULL_AIR};
use crate::render::texture::{compose_block_array_layers, generate_all_texture_layers_rgba};
use crate::ui::inventory::{InventoryScreen, InventoryTarget};
use crate::ui::item_icon::compose_item_sprite_layers;
use crate::world::block::BlockId;
use crate::world::chunk::{linear_index, world_to_chunk_and_local, Chunk, ChunkPos, CHUNK_SIZE};
use crate::world::daycycle::DEFAULT_TIME_OF_DAY;
use crate::world::manager::{MAX_CHUNK_Y, MIN_CHUNK_Y};
use crate::world::persistence::{
    load_world_from_path_with, save_world_to_path_with, ChunkEditMap, SavedPlayerState,
    WorldSaveData,
};
use crate::world::season::Season;
use crate::world::session::WorldSession;
use crate::world::terrain::{
    biome_at_seeded, generate_chunk_seeded, generate_chunk_seeded_with, terrain_height_at_seeded,
    Biome,
};

// ---------------------------------------------------------------------------
// Headless presentation digest (`--demo-presentation`, M010/S04-T05)
// ---------------------------------------------------------------------------

/// Fixed seed of the presentation digest's fixture world: the authored blocks
/// sit relative to this seed's deterministic terrain height, so two machines
/// render the same world. Never a clock, a random source, or a path.
pub const PRESENTATION_DEMO_SEED: u32 = 0x0BEE_F5ED;

/// The fixture world's authored block positions: two floating cells above the
/// deterministic surface at column `(4, 4)`, far enough apart that greedy
/// meshing cannot merge one block's faces into the other's.
pub fn presentation_demo_block_positions() -> (IVec3, IVec3) {
    let surface = terrain_height_at_seeded(4, 4, PRESENTATION_DEMO_SEED);
    (IVec3::new(4, surface + 2, 4), IVec3::new(6, surface + 2, 6))
}

/// The stable label of an [`ItemId`] for the digest. Registry identities are
/// named through the same frozen registry the rest of the digest reports, so
/// the printed drop name is the authored name, never a display label.
pub fn presentation_item_label(registry: &ContentRegistry, id: ItemId) -> String {
    match id {
        ItemId::Block(block) => registry
            .block_name(block)
            .map(str::to_string)
            .unwrap_or_else(|| format!("block:{}", block.to_raw())),
        ItemId::Item(index) => registry
            .item_def_name(index)
            .map(str::to_string)
            .unwrap_or_else(|| format!("item:{index}")),
        ItemId::ModTool(index) => registry
            .tool_def_name(index)
            .map(str::to_string)
            .unwrap_or_else(|| format!("tool:{index}")),
        ItemId::Material(material) => material.name().to_string(),
        ItemId::Tool { kind, tier } => format!("{}_{}", tier.name(), kind.name()),
    }
}

/// The v8 fixture save the digest writes and reloads: a frozen seed, two
/// authored block edits in the chunk at the origin column, one authored item
/// stack in the hotbar, and a Creative player standing just above them.
///
/// Everything is derived from the frozen seed, so the file bytes are stable
/// across machines for one registry (the v8 content table names the ids, and
/// the edit map is sorted by the encoder).
pub fn presentation_demo_save_data(ore: BlockId, block: BlockId, item_index: u16) -> WorldSaveData {
    let (ore_pos, block_pos) = presentation_demo_block_positions();
    let mut chunk_edits = ChunkEditMap::default();
    for (pos, id) in [(ore_pos, ore), (block_pos, block)] {
        let (chunk_pos, (lx, ly, lz)) = world_to_chunk_and_local(pos.x, pos.y, pos.z);
        chunk_edits
            .entry(chunk_pos)
            .or_default()
            .insert(linear_index(lx, ly, lz) as u16, id);
    }
    let mut inventory = Inventory::empty();
    inventory
        .insert(Item::new(ItemId::Item(item_index), 5).expect("fixture item stack"))
        .expect("the empty fixture inventory accepts the stack");
    let eye_y = (ore_pos.y + 4) as f32;
    WorldSaveData {
        seed: PRESENTATION_DEMO_SEED,
        time_of_day: DEFAULT_TIME_OF_DAY,
        day_index: 0,
        season_length_days: crate::world::season::DEFAULT_SEASON_LENGTH_DAYS,
        mode: GameMode::Creative,
        player: SavedPlayerState {
            position: [4.5, eye_y, 4.5],
            yaw: 0.0,
            pitch: -0.4,
            fly_mode: true,
            selected_slot: 0,
            render_distance: 16,
            inventory,
            player_grid: CraftingGrid::new(GridKind::Player),
            table_grid: CraftingGrid::new(GridKind::Table),
            survival: SurvivalState::full(),
        },
        chests: Default::default(),
        mobs: Vec::new(),
        chunk_edits,
    }
}

/// Build the deterministic `--demo-presentation` digest.
///
/// Every line is a structural fact (counts, resolved names, authored layers,
/// booleans) rather than a timing, an absolute path or an asset hash, so the
/// pinned line set is machine-independent. All content comes through the
/// production seams: the frozen session registry, the CPU composers the real
/// renderer uploads, `mesh_chunk_with`, `MiningTarget::from_block_with` +
/// `MiningState::tick_with`, `break_mined_block`, and the save/load codec.
///
/// Returns `(lines, ok)`. `ok` is false exactly when a leg recorded a
/// `failed=<name>` token; the caller exits non-zero on that flag.
pub fn presentation_demo_lines(
    registry: &Arc<ContentRegistry>,
    fixture: Option<&Path>,
) -> (Vec<String>, bool) {
    use crate::content::faces::BlockFaces;
    use crate::content::{
        PACK_ICON_BASE, PACK_LAYER_BASE, PLACEHOLDER_BLOCK_LAYER, PLACEHOLDER_ICON_LAYER,
    };
    use crate::render::texture::{BLOCK_ARRAY_LAYERS, NUM_TEX_LAYERS};
    use crate::ui::item_icon::{
        generate_item_sprite_layers_rgba, ITEM_SPRITE_ARRAY_LAYERS, ITEM_SPRITE_BYTES,
    };

    let mut lines: Vec<String> = Vec::new();
    let mut failed: Option<&'static str> = None;

    // (a) Frozen registry counts and the two composed GPU arrays.
    lines.push(format!(
        "presentation_demo: leg=registry blocks={} items={} pack_layers={} pack_icons={}",
        registry.blocks().len(),
        registry.items().len(),
        registry.pack_layers().len(),
        registry.pack_icons().len(),
    ));

    let base_layers = generate_all_texture_layers_rgba();
    let block_array = compose_block_array_layers(&base_layers, registry.pack_layers());
    let empty_block_array = compose_block_array_layers(&base_layers, &[]);
    let builtin_layers_match = block_array.len() == BLOCK_ARRAY_LAYERS as usize
        && block_array
            .iter()
            .take(NUM_TEX_LAYERS as usize)
            .eq(base_layers.iter().take(NUM_TEX_LAYERS as usize));
    let pack_layer_match = match registry.pack_layer(PACK_LAYER_BASE) {
        Some(layer) => block_array.get(PACK_LAYER_BASE as usize).map(Vec::as_slice) == Some(layer),
        None => {
            block_array.get(PACK_LAYER_BASE as usize)
                == empty_block_array.get(PACK_LAYER_BASE as usize)
        }
    };
    lines.push(format!(
        "presentation_demo: leg=block_array layers={} builtin_match={} pack_match={}",
        block_array.len(),
        builtin_layers_match,
        pack_layer_match,
    ));
    if !builtin_layers_match {
        failed.get_or_insert("block_array_builtin");
    }
    if !pack_layer_match {
        failed.get_or_insert("block_array_pack");
    }

    let base_icons = generate_item_sprite_layers_rgba();
    let item_atlas = compose_item_sprite_layers(registry.pack_icons());
    let builtin_icons_match = item_atlas.len() == ITEM_SPRITE_ARRAY_LAYERS as usize
        && item_atlas
            .iter()
            .take(base_icons.len())
            .eq(base_icons.iter());
    let padding_icon = vec![0u8; ITEM_SPRITE_BYTES];
    let pack_icon_match = match registry.pack_icon(PACK_ICON_BASE) {
        Some(icon) => item_atlas.get(PACK_ICON_BASE as usize).map(Vec::as_slice) == Some(icon),
        None => item_atlas.get(PACK_ICON_BASE as usize) == Some(&padding_icon),
    };
    lines.push(format!(
        "presentation_demo: leg=item_atlas layers={} builtin_match={} pack_match={}",
        item_atlas.len(),
        builtin_icons_match,
        pack_icon_match,
    ));
    if !builtin_icons_match {
        failed.get_or_insert("item_atlas_builtin");
    }
    if !pack_icon_match {
        failed.get_or_insert("item_atlas_pack");
    }

    // (b) Authored content lookups. A stack with no packs at all is the
    // built-in launch: nothing authored is expected, so only the absence is
    // reported. A non-empty stack that cannot resolve the pinned names is a
    // broken pack and fails closed.
    let authored_ore = registry.block_by_name("mymod:ruby_ore");
    let authored_block = registry.block_by_name("mymod:ruby_block");
    let authored_item = registry.item_by_name("mymod:ruby");
    let has_packs = !registry.packs().is_empty();

    if !has_packs {
        lines.push(
            "presentation_demo: leg=authored ruby_ore=absent ruby_block=absent ruby=absent"
                .to_string(),
        );
    } else if authored_ore.is_none() || authored_block.is_none() || authored_item.is_none() {
        if authored_ore.is_none() {
            failed.get_or_insert("ruby_ore");
        }
        if authored_block.is_none() {
            failed.get_or_insert("ruby_block");
        }
        if authored_item.is_none() {
            failed.get_or_insert("ruby_item");
        }
        lines.push(format!(
            "presentation_demo: leg=authored ruby_ore={} ruby_block={} ruby={}",
            if authored_ore.is_some() {
                "present"
            } else {
                "absent"
            },
            if authored_block.is_some() {
                "present"
            } else {
                "absent"
            },
            if authored_item.is_some() {
                "present"
            } else {
                "absent"
            },
        ));
    } else {
        let ore = authored_ore.expect("checked present above");
        let block = authored_block.expect("checked present above");
        let item = authored_item.expect("checked present above");

        for def in [ore, block] {
            let faces = def
                .faces
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let drop = match def.drop.and_then(ItemRef::item_id) {
                Some(id) => presentation_item_label(registry, id),
                None => "none".to_string(),
            };
            let hardness = match def.hardness {
                Some(value) => format!("{value:.3}"),
                None => "none".to_string(),
            };
            lines.push(format!(
                "presentation_demo: leg=block name={} id={} faces={} hardness={} drop={} stack={}",
                def.name,
                def.id.to_raw(),
                faces,
                hardness,
                drop,
                def.stack_limit,
            ));
        }
        lines.push(format!(
            "presentation_demo: leg=item name={} id={} icon={} stack={}",
            item.name, item.index, item.icon_layer, item.stack_limit,
        ));

        // (c) Mesh the authored blocks through the production face table.
        let mut session = WorldSession::new_session_with_mode_and_registry(
            PRESENTATION_DEMO_SEED,
            fixture
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("presentation-demo.mcvx")),
            crate::world::season::DEFAULT_SEASON_LENGTH_DAYS,
            GameMode::Creative,
            Arc::clone(registry),
        );
        let chunk_pos = ChunkPos::new(0, 0, 0);
        session.world.insert_chunk_for_test(
            chunk_pos,
            crate::world::chunk::Chunk::new_uniform(BlockId::Air),
        );
        for neighbor in [
            ChunkPos::new(1, 0, 0),
            ChunkPos::new(-1, 0, 0),
            ChunkPos::new(0, 1, 0),
            ChunkPos::new(0, -1, 0),
            ChunkPos::new(0, 0, 1),
            ChunkPos::new(0, 0, -1),
        ] {
            session.world.insert_chunk_for_test(
                neighbor,
                crate::world::chunk::Chunk::new_uniform(BlockId::Air),
            );
        }
        let ore_pos = IVec3::new(0, 10, 0);
        let block_pos = IVec3::new(2, 10, 0);
        session
            .world
            .set_block_immediate(ore_pos.x, ore_pos.y, ore_pos.z, ore.id);
        session
            .world
            .set_block_immediate(block_pos.x, block_pos.y, block_pos.z, block.id);

        let mut mesh_ok = false;
        if let Some(padded) = session.world.build_padded_chunk(chunk_pos) {
            let mesh = crate::mesh::greedy::mesh_chunk_with(&padded, session.world.block_faces());
            let mut layers: Vec<u8> = mesh
                .buckets
                .iter()
                .flat_map(|bucket| bucket.iter())
                .map(|quad| quad.unpack().6)
                .collect();
            layers.sort_unstable();
            layers.dedup();
            let mut expected: Vec<u8> = ore
                .faces
                .iter()
                .chain(block.faces.iter())
                .copied()
                .collect();
            expected.sort_unstable();
            expected.dedup();
            let layer_text = layers
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(",");
            lines.push(format!(
                "presentation_demo: leg=mesh blocks=2 quads={} layers={}",
                mesh.total_quads(),
                layer_text,
            ));
            // Two isolated blocks with six exposed faces each: exactly 12
            // greedy quads, and every one samples an authored layer.
            mesh_ok = mesh.total_quads() == 12 && layers == expected;
        }
        if !mesh_ok {
            failed.get_or_insert("mesh");
        }

        // (d) Mine the authored block through the production break path.
        let target = MiningTarget::from_block_with(&session.registry, ore_pos, ore.id, None);
        let mut ticks = 0usize;
        let mut broke = false;
        if let Some(target) = target {
            let mut mining = MiningState::new();
            while ticks < 10_000 {
                ticks += 1;
                match mining.tick_with(&session.registry, 0.05, Some(target)) {
                    MiningTick::Broke { .. } => {
                        broke = true;
                        break;
                    }
                    MiningTick::Mining { .. } => {}
                    MiningTick::Idle => break,
                }
            }
        }
        let outcome = break_mined_block(&mut session, ore_pos, ore.id);
        let applied = matches!(outcome, BreakOutcome::Applied { .. });
        let drop_entry = registry.drop_entry(ore.id);
        let collected = session
            .player
            .inventory()
            .slots()
            .iter()
            .enumerate()
            .find_map(|(slot, item)| item.map(|item| (slot, item)));
        let (drop_name, count, slot, limit) = match (drop_entry, collected) {
            (Some((id, limit)), Some((slot, item))) => (
                presentation_item_label(registry, id),
                item.count(),
                slot,
                limit,
            ),
            _ => ("none".to_string(), 0, 0, 0),
        };
        lines.push(format!(
            "presentation_demo: leg=mine target={} ticks={} broken={} applied={} drop={} count={} slot={} limit={}",
            ore.name, ticks, broke, applied, drop_name, count, slot, limit,
        ));
        let authored_drop = drop_entry
            .map(|(id, _)| id == ItemId::Item(item.index))
            .unwrap_or(false);
        let mine_ok = target.is_some()
            && broke
            && applied
            && authored_drop
            && count == 1
            && limit == item.stack_limit;
        if !mine_ok {
            failed.get_or_insert("mine");
        }

        // (e) Write the v8 fixture, reload it against the same registry, then
        // reload the same bytes against the built-in registry to prove the
        // reserved placeholder policy.
        let (fixture_path, remove_fixture) = match fixture {
            Some(path) => (path.to_path_buf(), false),
            None => (
                std::env::temp_dir().join(format!(
                    "opencraft-presentation-demo-{}.mcvx",
                    std::process::id()
                )),
                true,
            ),
        };
        let save_data = presentation_demo_save_data(ore.id, block.id, item.index);
        let saved = save_world_to_path_with(&fixture_path, &save_data, registry.as_ref());
        if saved.is_err() {
            failed.get_or_insert("save_write");
        } else {
            match load_world_from_path_with(&fixture_path, registry.as_ref()) {
                Ok(loaded) => {
                    let ore_resolved = loaded
                        .registry
                        .block_by_name("mymod:ruby_ore")
                        .map(|def| !def.placeholder)
                        .unwrap_or(false);
                    let block_resolved = loaded
                        .registry
                        .block_by_name("mymod:ruby_block")
                        .map(|def| !def.placeholder)
                        .unwrap_or(false);
                    let item_resolved = loaded
                        .registry
                        .item_by_name("mymod:ruby")
                        .map(|def| !def.placeholder)
                        .unwrap_or(false);
                    lines.push(format!(
                        "presentation_demo: leg=save unresolved={} ruby_ore={} ruby_block={} ruby_item={}",
                        loaded.unresolved, ore_resolved, block_resolved, item_resolved,
                    ));
                    if loaded.unresolved != 0 || !ore_resolved || !block_resolved || !item_resolved
                    {
                        failed.get_or_insert("save_reload");
                    }
                }
                Err(_) => {
                    failed.get_or_insert("save_reload");
                    lines.push("presentation_demo: leg=save unresolved=- ruby_ore=false ruby_block=false ruby_item=false".to_string());
                }
            }

            // Placeholder policy: the same bytes against built-in content.
            match load_world_from_path_with(&fixture_path, ContentRegistry::builtin()) {
                Ok(loaded) => {
                    let unresolved_placeholders = loaded.unresolved;
                    let placeholder = loaded.registry.block_by_name("mymod:ruby_ore");
                    let placeholder_item = loaded.registry.item_by_name("mymod:ruby");
                    let faces = BlockFaces::from_registry(&loaded.registry);
                    let mesh_layer = placeholder
                        .map(|def| faces.layer(def.id, 0))
                        .unwrap_or(u8::MAX);
                    let face = placeholder.map(|def| def.faces[0]).unwrap_or(u8::MAX);
                    let icon = placeholder_item
                        .map(|def| def.icon_layer)
                        .unwrap_or(u8::MAX);
                    let breakable = placeholder
                        .map(|def| def.hardness.is_none())
                        .unwrap_or(false);
                    lines.push(format!(
                        "presentation_demo: leg=placeholder unresolved={} face={} icon={} break={} mesh={}",
                        unresolved_placeholders,
                        face,
                        icon,
                        if breakable { "none" } else { "some" },
                        mesh_layer,
                    ));
                    let placeholder_ok = unresolved_placeholders == 3
                        && face == PLACEHOLDER_BLOCK_LAYER
                        && icon == PLACEHOLDER_ICON_LAYER
                        && breakable
                        && mesh_layer == PLACEHOLDER_BLOCK_LAYER;
                    if !placeholder_ok {
                        failed.get_or_insert("placeholder");
                    }
                }
                Err(_) => {
                    failed.get_or_insert("placeholder_reload");
                    lines.push("presentation_demo: leg=placeholder unresolved=- face=- icon=- break=some mesh=-".to_string());
                }
            }
        }
        if remove_fixture {
            let _ = std::fs::remove_file(&fixture_path);
            let _ = std::fs::remove_file(fixture_path.with_extension("mcvx.tmp"));
        }
    }

    // The empty-stack (built-in) launch still proves the reserved policy on a
    // deterministic probe, so the placeholder contract is checked even when
    // no pack content exists to save.
    if !has_packs {
        let mut probe = registry.as_ref().clone();
        let probe_block = probe.append_placeholder_block("mymod:probe_block");
        let probe_item = probe.append_placeholder_item("mymod:probe_item");
        let probe_faces = BlockFaces::from_registry(&probe);
        let placeholder = probe
            .block(probe_block)
            .expect("appended placeholder block");
        let placeholder_item = probe.item(probe_item).expect("appended placeholder item");
        let mesh_layer = probe_faces.layer(probe_block, 0);
        let placeholder_ok = placeholder.placeholder
            && placeholder_item.placeholder
            && placeholder.faces == [PLACEHOLDER_BLOCK_LAYER; 6]
            && placeholder_item.icon_layer == PLACEHOLDER_ICON_LAYER
            && placeholder.hardness.is_none()
            && mesh_layer == PLACEHOLDER_BLOCK_LAYER;
        lines.push(format!(
            "presentation_demo: leg=placeholder unresolved=0 face={} icon={} break={} mesh={}",
            placeholder.faces[0],
            placeholder_item.icon_layer,
            if placeholder.hardness.is_none() {
                "none"
            } else {
                "some"
            },
            mesh_layer,
        ));
        if !placeholder_ok {
            failed.get_or_insert("placeholder");
        }
    }

    match failed {
        Some(name) => {
            lines.push(format!("presentation_demo: ok=false failed={name}"));
            (lines, false)
        }
        None => {
            lines.push("presentation_demo: ok=true".to_string());
            (lines, true)
        }
    }
}

// ---------------------------------------------------------------------------
// Headless crafting digest (`--demo-crafting`, M010/S05-T06)
// ---------------------------------------------------------------------------

/// Build a shaped grid from authored `rows` and a `char -> ItemId` key.
///
/// Every cell is constructed through [`Item::new_with`] so pack ingredients
/// carry their authored stack limits, exactly like a real pick-up.
pub fn crafting_demo_grid(
    registry: &ContentRegistry,
    kind: GridKind,
    rows: &[&str],
    key: &[(char, ItemId)],
) -> CraftingGrid {
    let mut grid = CraftingGrid::new(kind);
    let dim = kind.dim();
    for (row, line) in rows.iter().enumerate() {
        for (col, character) in line.chars().enumerate() {
            if character == ' ' {
                continue;
            }
            let id = key
                .iter()
                .find(|(key, _)| *key == character)
                .map(|(_, id)| *id)
                .expect("fixture pattern characters have a key");
            let item = Item::new_with(registry, id, 1).expect("fixture ingredient constructs");
            grid.set(row * dim + col, Some(item))
                .expect("fixture cell is inside the grid");
        }
    }
    grid
}

/// The stable authored name of an [`ItemId`], or `None` for an unknown index.
///
/// The digest prints the same names the registry resolved, never a display
/// label and never an opaque index.
pub fn crafting_demo_item_name(registry: &ContentRegistry, id: ItemId) -> Option<String> {
    match id {
        ItemId::Block(block) => registry.block_name(block).map(str::to_string),
        ItemId::ModTool(index) => registry.tool_def_name(index).map(str::to_string),
        ItemId::Item(index) => registry.item_def_name(index).map(str::to_string),
        other => Some(other.name().to_string()),
    }
}

/// Total item units across the 36 slots, the cursor, and both grids — the
/// conservation invariant a failed craft must preserve.
pub fn crafting_demo_units(player: &Player) -> u32 {
    let mut total = 0u32;
    for item in player.inventory().slots().iter().flatten() {
        total += u32::from(item.count());
    }
    if let Some(cursor) = player.inventory().cursor() {
        total += u32::from(cursor.count());
    }
    for grid in [player.player_grid(), player.table_grid()] {
        for index in 0..grid.kind().slot_count() {
            if let Some(item) = grid.slot(index) {
                total += u32::from(item.count());
            }
        }
    }
    total
}

/// Mine one authored block with `tool` through the production break path and
/// report `(ticks, remaining_durability)`.
///
/// The accumulator and the world edit are the same pair the frame loop uses:
/// [`MiningTarget::from_block_with`] + [`MiningState::tick_with`] for the
/// timing, then [`break_mined_block`] for the edit, wear and drop. A fresh
/// synthetic air chunk keeps the measurement independent of terrain.
pub fn crafting_demo_break(
    registry: &Arc<ContentRegistry>,
    block: BlockId,
    tool: Item,
) -> Option<(usize, u16)> {
    let mut session = WorldSession::new_session_with_mode_and_registry(
        0x00C0_FFEE,
        PathBuf::from("crafting-demo-break.mcvx"),
        crate::world::season::DEFAULT_SEASON_LENGTH_DAYS,
        GameMode::Creative,
        Arc::clone(registry),
    );
    let chunk_pos = ChunkPos::new(0, 0, 0);
    session.world.insert_chunk_for_test(
        chunk_pos,
        crate::world::chunk::Chunk::new_uniform(BlockId::Air),
    );
    for neighbor in [
        ChunkPos::new(1, 0, 0),
        ChunkPos::new(-1, 0, 0),
        ChunkPos::new(0, 1, 0),
        ChunkPos::new(0, -1, 0),
        ChunkPos::new(0, 0, 1),
        ChunkPos::new(0, 0, -1),
    ] {
        session.world.insert_chunk_for_test(
            neighbor,
            crate::world::chunk::Chunk::new_uniform(BlockId::Air),
        );
    }
    let ore_pos = IVec3::new(0, 10, 0);
    session
        .world
        .set_block_immediate(ore_pos.x, ore_pos.y, ore_pos.z, block);
    session.player.collect_item(tool);
    if !session.player.select_slot(0) {
        return None;
    }

    let target = MiningTarget::from_block_with(registry, ore_pos, block, Some(tool))?;
    let mut mining = MiningState::new();
    let mut ticks = 0usize;
    let mut broke = false;
    while ticks < 10_000 {
        ticks += 1;
        match mining.tick_with(registry, 0.05, Some(target)) {
            MiningTick::Broke { .. } => {
                broke = true;
                break;
            }
            MiningTick::Mining { .. } => {}
            MiningTick::Idle => return None,
        }
    }
    if !broke {
        return None;
    }

    match break_mined_block(&mut session, ore_pos, block) {
        BreakOutcome::Applied { wear, .. } => match wear {
            ToolWearOutcome::Worn { remaining } => Some((ticks, remaining)),
            ToolWearOutcome::Broke => Some((ticks, 0)),
            ToolWearOutcome::Nothing => None,
        },
        BreakOutcome::Rejected(_) => None,
    }
}

/// Build the deterministic `--demo-crafting` digest.
///
/// Every leg runs through a production function: the frozen registry resolved
/// from the launch stack, [`apply_inventory_click_button_with`], the mining
/// accumulator + [`break_mined_block`], [`EatingTarget::new_with`] +
/// [`EatingState::tick`] + [`Player::eat_selected_with`], and the save/load
/// codec. Each structural line is machine-independent (names, counts,
/// booleans) and the caller exits non-zero when the latch is `crafting:
/// failed`.
pub fn crafting_demo_lines(
    registry: &Arc<ContentRegistry>,
    fixture: Option<&Path>,
) -> (Vec<String>, bool) {
    let mut lines: Vec<String> = Vec::new();
    let mut failed: Option<&'static str> = None;

    // Resolve the authored identities once; every leg uses the same frozen
    // registry the launch would freeze.
    let ruby_index = registry.item_by_name("mymod:ruby").map(|def| def.index);
    let stew_index = registry
        .item_by_name("mymod:ruby_stew")
        .map(|def| def.index);
    let pickaxe_index = registry
        .tool_by_name("mymod:ruby_pickaxe")
        .map(|def| def.index);
    let ore = registry.block_by_name("mymod:ruby_ore").map(|def| def.id);

    // (a) Craft the ruby pickaxe at the 4x4 table through the production
    // result-slot seam.
    let table_craft = match (ruby_index, pickaxe_index) {
        (Some(ruby), Some(_pickaxe)) => {
            let ruby_item = Item::new_with(registry, ItemId::Item(ruby), 1)
                .expect("the authored ruby item constructs");
            let stick =
                Item::new(ItemId::Material(Material::Stick), 1).expect("a stick is carryable");
            let grid = crafting_demo_grid(
                registry,
                GridKind::Table,
                &["RRR", " S ", " S "],
                &[('R', ruby_item.id()), ('S', stick.id())],
            );
            let mut player = Player::with_inventory(
                Vec3::ZERO,
                Inventory::empty(),
                CraftingGrid::new(GridKind::Player),
                grid,
            );
            let mut screen = InventoryScreen::new();
            screen.open_table();
            let outcome = apply_inventory_click_button_with(
                &mut player,
                registry,
                &screen,
                Some(IVec3::new(0, 5, 0)),
                InventoryTarget::ResultSlot(GridKind::Table),
                InventoryClickButton::Left,
            );
            match outcome {
                Ok(InventoryClickOutcome::Crafted(item)) => {
                    let name = crafting_demo_item_name(registry, item.id())
                        .unwrap_or_else(|| "unresolved".to_string());
                    (
                        name,
                        item.count(),
                        item.durability().unwrap_or(0),
                        item.max_durability().unwrap_or(0),
                    )
                }
                _ => ("none".to_string(), 0, 0, 0),
            }
        }
        _ => ("none".to_string(), 0, 0, 0),
    };
    lines.push(format!(
        "crafting: leg=table item={} count={} durability={} max={}",
        table_craft.0, table_craft.1, table_craft.2, table_craft.3,
    ));
    if table_craft.0 != "mymod:ruby_pickaxe"
        || table_craft.1 != 1
        || table_craft.2 != 900
        || table_craft.3 != 900
    {
        failed.get_or_insert("table");
    }

    // (b) Craft the ruby stew at the 2x2 player grid.
    let stew_stack_limit = stew_index
        .and_then(|index| registry.item(index))
        .map(|def| def.stack_limit)
        .unwrap_or(0);
    let player_craft = match (ruby_index, stew_index) {
        (Some(ruby), Some(_stew)) => {
            let ruby_item = Item::new_with(registry, ItemId::Item(ruby), 1)
                .expect("the authored ruby item constructs");
            let wheat =
                Item::new(ItemId::Material(Material::Wheat), 1).expect("wheat is carryable");
            let grid = crafting_demo_grid(
                registry,
                GridKind::Player,
                &["RR", "WW"],
                &[('R', ruby_item.id()), ('W', wheat.id())],
            );
            let mut player = Player::with_inventory(
                Vec3::ZERO,
                Inventory::empty(),
                grid,
                CraftingGrid::new(GridKind::Table),
            );
            let mut screen = InventoryScreen::new();
            screen.open_player();
            let outcome = apply_inventory_click_button_with(
                &mut player,
                registry,
                &screen,
                None,
                InventoryTarget::ResultSlot(GridKind::Player),
                InventoryClickButton::Left,
            );
            match outcome {
                Ok(InventoryClickOutcome::Crafted(item)) => (
                    crafting_demo_item_name(registry, item.id())
                        .unwrap_or_else(|| "unresolved".to_string()),
                    item.count(),
                ),
                _ => ("none".to_string(), 0),
            }
        }
        _ => ("none".to_string(), 0),
    };
    lines.push(format!(
        "crafting: leg=player item={} count={} stack={}",
        player_craft.0, player_craft.1, stew_stack_limit,
    ));
    if player_craft.0 != "mymod:ruby_stew" || player_craft.1 != 1 || stew_stack_limit != 16 {
        failed.get_or_insert("player");
    }

    // (c) Break the ruby ore with ruby and iron; both durations and both worn
    // durabilities come from the production mining path.
    let mine = match (ore, pickaxe_index) {
        (Some(ore), Some(pickaxe)) => {
            let ruby_tool = Item::new_with(registry, ItemId::ModTool(pickaxe), 1)
                .expect("the authored pickaxe constructs");
            let iron_tool = Item::new(
                ItemId::Tool {
                    kind: ToolKind::Pickaxe,
                    tier: ToolTier::Iron,
                },
                1,
            )
            .expect("an iron pickaxe constructs");
            match (
                crafting_demo_break(registry, ore, ruby_tool),
                crafting_demo_break(registry, ore, iron_tool),
            ) {
                (Some((ruby_ticks, ruby_wear)), Some((iron_ticks, iron_wear))) => {
                    Some((ruby_ticks, iron_ticks, ruby_wear, iron_wear))
                }
                _ => None,
            }
        }
        _ => None,
    };
    let (ruby_ticks, iron_ticks, ruby_wear, iron_wear) = mine.unwrap_or((0, 0, 0, 0));
    lines.push(format!(
        "crafting: leg=mine block=mymod:ruby_ore ruby_ticks={} iron_ticks={} ruby_wear={} iron_wear={}",
        ruby_ticks, iron_ticks, ruby_wear, iron_wear,
    ));
    if !(ruby_ticks > 0 && ruby_ticks < iron_ticks && ruby_wear == 899 && iron_wear == 249) {
        failed.get_or_insert("mine");
    }

    // (d) Eat the stew through the real hold-to-eat flow.
    let eat = match stew_index {
        Some(stew) => {
            let stew_item = Item::new_with(registry, ItemId::Item(stew), 1)
                .expect("the authored stew constructs");
            let mut inventory = Inventory::empty();
            inventory
                .insert(stew_item)
                .expect("an empty inventory holds one stew");
            let mut player = Player::with_inventory(
                Vec3::ZERO,
                inventory,
                CraftingGrid::new(GridKind::Player),
                CraftingGrid::new(GridKind::Table),
            );
            player.survival =
                SurvivalState::new(20, 5, FULL_AIR).expect("5 hunger is a valid vitals value");
            player.select_slot(0);
            let mut eating = EatingState::new();
            let mut completed = None;
            for _ in 0..1_000 {
                let target =
                    EatingTarget::new_with(registry, 0, stew_item, player.survival.hunger());
                match eating.tick(0.05, target) {
                    EatingTick::Ate {
                        slot,
                        item,
                        restore,
                    } => {
                        completed = player
                            .eat_selected_with(registry, slot, item)
                            .map(|applied| (applied, restore));
                        break;
                    }
                    EatingTick::Eating { .. } => {}
                    EatingTick::Idle => break,
                }
            }
            completed.map(|(applied, _)| (applied, player.survival.hunger()))
        }
        None => None,
    };
    let (restored, hunger_after) = eat.unwrap_or((0, 0));
    lines.push(format!(
        "crafting: leg=eat item=mymod:ruby_stew restored={} hunger={}",
        restored, hunger_after,
    ));
    if restored != 7 || hunger_after != 12 {
        failed.get_or_insert("eat");
    }

    // (e) The disabled built-in shortbow has no preview and cannot craft.
    let shortbow = if registry
        .recipe_by_name("shortbow")
        .map(|recipe| !recipe.enabled)
        .unwrap_or(false)
    {
        let stick = Item::new(ItemId::Material(Material::Stick), 1).expect("a stick is carryable");
        let leather =
            Item::new(ItemId::Material(Material::Leather), 1).expect("leather is carryable");
        let grid = crafting_demo_grid(
            registry,
            GridKind::Table,
            &[" SE", "S E", " S "],
            &[('S', stick.id()), ('E', leather.id())],
        );
        let mut player = Player::with_inventory(
            Vec3::ZERO,
            Inventory::empty(),
            CraftingGrid::new(GridKind::Player),
            grid,
        );
        let preview = player
            .craft_preview_with(registry, GridKind::Table)
            .is_some();
        let mut screen = InventoryScreen::new();
        screen.open_table();
        let crafted = matches!(
            apply_inventory_click_button_with(
                &mut player,
                registry,
                &screen,
                Some(IVec3::new(0, 5, 0)),
                InventoryTarget::ResultSlot(GridKind::Table),
                InventoryClickButton::Left,
            ),
            Ok(InventoryClickOutcome::Crafted(_))
        );
        (preview, crafted)
    } else {
        (true, true)
    };
    lines.push(format!(
        "crafting: leg=shortbow preview={} craft={}",
        shortbow.0, shortbow.1,
    ));
    if shortbow.0 || shortbow.1 {
        failed.get_or_insert("shortbow");
    }

    // (f) A grid missing one ingredient never matches and consumes nothing.
    let missing = match (ruby_index, pickaxe_index) {
        (Some(ruby), Some(_)) => {
            let ruby_item = Item::new_with(registry, ItemId::Item(ruby), 1)
                .expect("the authored ruby item constructs");
            let stick =
                Item::new(ItemId::Material(Material::Stick), 1).expect("a stick is carryable");
            // The pickaxe pattern with the middle stick omitted.
            let grid = crafting_demo_grid(
                registry,
                GridKind::Table,
                &["RRR", "   ", " S "],
                &[('R', ruby_item.id()), ('S', stick.id())],
            );
            let mut player = Player::with_inventory(
                Vec3::ZERO,
                Inventory::empty(),
                CraftingGrid::new(GridKind::Player),
                grid,
            );
            let before = crafting_demo_units(&player);
            let preview = player
                .craft_preview_with(registry, GridKind::Table)
                .is_some();
            let mut screen = InventoryScreen::new();
            screen.open_table();
            let crafted = matches!(
                apply_inventory_click_button_with(
                    &mut player,
                    registry,
                    &screen,
                    Some(IVec3::new(0, 5, 0)),
                    InventoryTarget::ResultSlot(GridKind::Table),
                    InventoryClickButton::Left,
                ),
                Ok(InventoryClickOutcome::Crafted(_))
            );
            let consumed = before.saturating_sub(crafting_demo_units(&player));
            (preview, crafted, consumed)
        }
        _ => (true, true, 1),
    };
    lines.push(format!(
        "crafting: leg=missing preview={} craft={} consumed={}",
        missing.0, missing.1, missing.2,
    ));
    if missing.0 || missing.1 || missing.2 != 0 {
        failed.get_or_insert("missing");
    }

    // (g)/(h) Save and reload the ruby pickaxe, then reload the same bytes
    // with no pack at all.
    let (fixture_path, remove_fixture) = match fixture {
        Some(path) => (path.to_path_buf(), false),
        None => (
            std::env::temp_dir().join(format!(
                "opencraft-crafting-demo-{}.mcvx",
                std::process::id()
            )),
            true,
        ),
    };
    let mut reloaded = (0u16, usize::MAX, false);
    let mut placeholder = (0usize, 0u16, false);
    if let Some(pickaxe) = pickaxe_index {
        match Item::tool_with_durability_with(registry, ItemId::ModTool(pickaxe), 850) {
            Ok(tool) => {
                let mut session = WorldSession::new_session_with_mode_and_registry(
                    0x00C0_FFEE,
                    fixture_path.clone(),
                    crate::world::season::DEFAULT_SEASON_LENGTH_DAYS,
                    GameMode::Creative,
                    Arc::clone(registry),
                );
                session.player.collect_item(tool);
                session.player.select_slot(0);
                if save_current_world(&mut session).is_err() {
                    failed.get_or_insert("save_write");
                } else {
                    match load_world_from_path_with(&fixture_path, registry.as_ref()) {
                        Ok(loaded) => {
                            let resolved = loaded
                                .data
                                .player
                                .inventory
                                .slot(0)
                                .map(|item| (item.durability().unwrap_or(0), item.id()));
                            let name_ok = resolved
                                .and_then(|(_, id)| crafting_demo_item_name(registry, id))
                                .map(|name| name == "mymod:ruby_pickaxe")
                                .unwrap_or(false);
                            reloaded = (
                                resolved.map(|(durability, _)| durability).unwrap_or(0),
                                loaded.unresolved,
                                name_ok,
                            );
                        }
                        Err(_) => {
                            failed.get_or_insert("save_reload");
                        }
                    }

                    match load_world_from_path_with(&fixture_path, ContentRegistry::builtin()) {
                        Ok(loaded) => {
                            let item = loaded.data.player.inventory.slot(0);
                            let is_placeholder = item
                                .and_then(|item| match item.id() {
                                    ItemId::Item(index) => {
                                        loaded.registry.item(index).map(|def| def.placeholder)
                                    }
                                    _ => Some(false),
                                })
                                .unwrap_or(false);
                            placeholder = (
                                loaded.unresolved,
                                item.map(|item| item.count()).unwrap_or(0),
                                is_placeholder,
                            );
                        }
                        Err(_) => {
                            failed.get_or_insert("placeholder_reload");
                        }
                    }
                }
            }
            Err(_) => {
                failed.get_or_insert("save_construct");
            }
        }
    } else {
        failed.get_or_insert("save_construct");
    }
    lines.push(format!(
        "crafting: leg=save durability={} unresolved={} resolved={}",
        reloaded.0, reloaded.1, reloaded.2,
    ));
    if reloaded.0 != 850 || reloaded.1 != 0 || !reloaded.2 {
        failed.get_or_insert("save");
    }
    lines.push(format!(
        "crafting: leg=placeholder name=mymod:ruby_pickaxe unresolved={} count={} placeholder={}",
        placeholder.0, placeholder.1, placeholder.2,
    ));
    if placeholder.0 != 1 || placeholder.1 != 1 || !placeholder.2 {
        failed.get_or_insert("placeholder");
    }
    if remove_fixture {
        let _ = std::fs::remove_file(&fixture_path);
        let _ = std::fs::remove_file(fixture_path.with_extension("mcvx.tmp"));
    }

    match failed {
        Some(name) => {
            lines.push(format!("crafting: failed reason={name}"));
            (lines, false)
        }
        None => {
            lines.push("crafting: ok".to_string());
            (lines, true)
        }
    }
}

/// Seed pinned for the `--demo-worldgen` digest.
///
/// A plain constant (never a clock or `random_seed`) so the digest is a pure
/// function of the checked-in pack plus this seed.
pub const WORLDGEN_DEMO_SEED: u32 = 424_242;

/// The 8x8 chunk-column window the digest scans: `x in -1..=6`,
/// `z in -1..=6`, across every world chunk layer (`MIN_CHUNK_Y..=MAX_CHUNK_Y`,
/// six layers) — 384 chunk generations per leg. Wide enough to hold authored
/// ore and patch cells at the pinned seed (the 4x4 window had no patch hit)
/// while staying bounded and deterministic.
pub const WORLDGEN_DEMO_MIN_CX: i32 = -1;
pub const WORLDGEN_DEMO_MAX_CX: i32 = 6;
pub const WORLDGEN_DEMO_MIN_CZ: i32 = -1;
pub const WORLDGEN_DEMO_MAX_CZ: i32 = 6;

/// Generate the pinned digest window with `rules` through the production seam.
pub fn worldgen_demo_window(rules: &[WorldgenRule]) -> Vec<(ChunkPos, Chunk)> {
    let mut chunks = Vec::new();
    for cz in WORLDGEN_DEMO_MIN_CZ..=WORLDGEN_DEMO_MAX_CZ {
        for cx in WORLDGEN_DEMO_MIN_CX..=WORLDGEN_DEMO_MAX_CX {
            for cy in MIN_CHUNK_Y..=MAX_CHUNK_Y {
                let pos = ChunkPos::new(cx, cy, cz);
                chunks.push((
                    pos,
                    generate_chunk_seeded_with(pos, WORLDGEN_DEMO_SEED, Season::Summer, rules),
                ));
            }
        }
    }
    chunks
}

/// Every world cell in `chunks` holding `block` for which `keep` is true.
pub fn worldgen_demo_cells(
    chunks: &[(ChunkPos, Chunk)],
    block: BlockId,
    keep: impl Fn(i32, i32, i32) -> bool,
) -> Vec<(i32, i32, i32)> {
    let mut cells = Vec::new();
    for (pos, chunk) in chunks {
        let base_x = pos.x * CHUNK_SIZE as i32;
        let base_y = pos.y * CHUNK_SIZE as i32;
        let base_z = pos.z * CHUNK_SIZE as i32;
        for ly in 0..CHUNK_SIZE {
            for lz in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    if chunk.get(lx, ly, lz) != block {
                        continue;
                    }
                    let wx = base_x + lx as i32;
                    let wy = base_y + ly as i32;
                    let wz = base_z + lz as i32;
                    if keep(wx, wy, wz) {
                        cells.push((wx, wy, wz));
                    }
                }
            }
        }
    }
    cells
}

/// Whether two chunks hold the same block in every cell.
pub fn worldgen_demo_chunks_equal(a: &Chunk, b: &Chunk) -> bool {
    (0..CHUNK_SIZE)
        .all(|y| (0..CHUNK_SIZE).all(|z| (0..CHUNK_SIZE).all(|x| a.get(x, y, z) == b.get(x, y, z))))
}

/// Stable lowercase name for an authored patch biome.
pub fn worldgen_demo_biome_name(biome: Biome) -> &'static str {
    match biome {
        Biome::Plains => "plains",
        Biome::Forest => "forest",
        Biome::Mountains => "mountains",
        Biome::Ocean => "ocean",
    }
}

/// Build the deterministic `--demo-worldgen` digest.
///
/// Every leg runs through a production seam: [`ContentRegistry::worldgen_rules`],
/// [`WorldSession::new_session_with_mode_and_registry`],
/// [`generate_chunk_seeded_with`] and [`generate_chunk_seeded`]. Each line is
/// machine-independent (names, counts, booleans), so `scripts/worldgen_smoke.sh`
/// can pin them whole-line. The caller exits non-zero when the latch fails.
pub fn worldgen_demo_lines(registry: &Arc<ContentRegistry>) -> (Vec<String>, bool) {
    let mut lines: Vec<String> = Vec::new();
    let mut failed: Option<&'static str> = None;

    let rules = registry.worldgen_rules();
    let ore_rules = rules
        .iter()
        .filter(|rule| matches!(rule, WorldgenRule::Ore { .. }))
        .count();
    let patch_rules = rules
        .iter()
        .filter(|rule| matches!(rule, WorldgenRule::Patch { .. }))
        .count();
    lines.push(format!(
        "worldgen_demo: leg=registry rules={} ore={} patch={}",
        rules.len(),
        ore_rules,
        patch_rules,
    ));
    if rules.len() != 2 || ore_rules != 1 || patch_rules != 1 {
        failed.get_or_insert("registry");
    }

    // Authored bands and biomes, read from the same frozen rules the generator
    // consumes. A missing or malformed pair fails the corresponding leg below.
    let mut ore_band = None;
    let mut patch_band = None;
    let mut patch_biomes: Vec<Biome> = Vec::new();
    for rule in rules {
        match rule {
            WorldgenRule::Ore { min_y, max_y, .. } => ore_band = Some((*min_y, *max_y)),
            WorldgenRule::Patch {
                biomes,
                min_y,
                max_y,
                ..
            } => {
                patch_band = Some((*min_y, *max_y));
                patch_biomes = biomes.clone();
            }
        }
    }

    // (a) The session seam: a registry-built session must expose exactly the
    // frozen registry's rules on its WorldManager (proves T02 wiring without
    // touching the async streaming loop).
    let session_path = std::env::temp_dir().join(format!(
        "opencraft-worldgen-demo-{}.mcvx",
        std::process::id()
    ));
    let session = WorldSession::new_session_with_mode_and_registry(
        WORLDGEN_DEMO_SEED,
        session_path,
        crate::world::season::DEFAULT_SEASON_LENGTH_DAYS,
        GameMode::Creative,
        Arc::clone(registry),
    );
    let session_rules = session.world.worldgen_rules();
    let session_matches = session_rules.as_ref() == rules;
    lines.push(format!(
        "worldgen_demo: leg=session rules={} matches={}",
        session_rules.len(),
        session_matches,
    ));
    if !session_matches {
        failed.get_or_insert("session");
    }

    let rule_chunks = worldgen_demo_window(rules);
    let authored_block = registry.block_by_name("mymod:ruby_ore").map(|def| def.id);

    // (b) Ore leg: authored ore below the surface must exist only inside the
    // authored y band and strictly inside the stone band (above bedrock, at
    // least four blocks below the column surface). Surface cells are excluded
    // here — the patch leg owns those.
    let mut ore_cells: Vec<(i32, i32, i32)> = Vec::new();
    if let Some(block) = authored_block {
        ore_cells = worldgen_demo_cells(&rule_chunks, block, |wx, wy, wz| {
            wy < terrain_height_at_seeded(wx, wz, WORLDGEN_DEMO_SEED)
        });
    }
    let ore_observed_min = ore_cells.iter().map(|cell| cell.1).min();
    let ore_observed_max = ore_cells.iter().map(|cell| cell.1).max();
    let ore_in_band = match (ore_band, ore_observed_min, ore_observed_max) {
        (Some((min_y, max_y)), Some(observed_min), Some(observed_max)) => {
            !ore_cells.is_empty()
                && min_y <= observed_min
                && observed_max <= max_y
                && ore_cells.iter().all(|&(wx, wy, wz)| {
                    wy > -32 && wy < terrain_height_at_seeded(wx, wz, WORLDGEN_DEMO_SEED) - 3
                })
        }
        _ => false,
    };
    lines.push(format!(
        "worldgen_demo: leg=ore block=mymod:ruby_ore cells={} min_y={} max_y={} in_band={}",
        ore_cells.len(),
        ore_observed_min.unwrap_or(0),
        ore_observed_max.unwrap_or(0),
        ore_in_band,
    ));
    if !ore_in_band {
        failed.get_or_insert("ore");
    }

    // (c) Patch leg: the authored block at `wy == h` is unambiguously a patch
    // cell (ore never reaches the surface), so every such cell must sit in an
    // authored biome and inside the authored band.
    let mut patch_cells: Vec<(i32, i32, i32)> = Vec::new();
    if let Some(block) = authored_block {
        patch_cells = worldgen_demo_cells(&rule_chunks, block, |wx, wy, wz| {
            wy == terrain_height_at_seeded(wx, wz, WORLDGEN_DEMO_SEED)
        });
    }
    let patch_biomes_ok = !patch_cells.is_empty()
        && !patch_biomes.is_empty()
        && patch_cells.iter().all(|&(wx, _, wz)| {
            patch_biomes.contains(&biome_at_seeded(wx, wz, WORLDGEN_DEMO_SEED))
        })
        && patch_cells.iter().all(|&(_, wy, _)| {
            patch_band
                .map(|(min_y, max_y)| (min_y..=max_y).contains(&wy))
                .unwrap_or(false)
        });
    let patch_biome_list = patch_biomes
        .iter()
        .map(|biome| worldgen_demo_biome_name(*biome))
        .collect::<Vec<_>>()
        .join(",");
    lines.push(format!(
        "worldgen_demo: leg=patch block=mymod:ruby_ore cells={} biomes={} biomes_ok={}",
        patch_cells.len(),
        patch_biome_list,
        patch_biomes_ok,
    ));
    if !patch_biomes_ok {
        failed.get_or_insert("patch");
    }

    // (d) Coal/iron equality: pack rules must never move a built-in ore cell.
    let empty_chunks = worldgen_demo_window(&[]);
    let coal = worldgen_demo_cells(&rule_chunks, BlockId::CoalOre, |_, _, _| true);
    let iron = worldgen_demo_cells(&rule_chunks, BlockId::IronOre, |_, _, _| true);
    let mut coal_sorted = coal.clone();
    coal_sorted.sort_unstable();
    let mut iron_sorted = iron.clone();
    iron_sorted.sort_unstable();
    let mut empty_coal = worldgen_demo_cells(&empty_chunks, BlockId::CoalOre, |_, _, _| true);
    empty_coal.sort_unstable();
    let mut empty_iron = worldgen_demo_cells(&empty_chunks, BlockId::IronOre, |_, _, _| true);
    empty_iron.sort_unstable();
    let unchanged = coal_sorted == empty_coal && iron_sorted == empty_iron;
    lines.push(format!(
        "worldgen_demo: leg=coal_iron coal={} iron={} unchanged={}",
        coal.len(),
        iron.len(),
        unchanged,
    ));
    if !unchanged {
        failed.get_or_insert("coal_iron");
    }

    // (e) Empty-rule identity: the wrapper must reproduce the pre-M010 chunks.
    let mut identical = true;
    for (pos, chunk) in &empty_chunks {
        let legacy = generate_chunk_seeded(*pos, WORLDGEN_DEMO_SEED, Season::Summer);
        if !worldgen_demo_chunks_equal(chunk, &legacy) {
            identical = false;
            break;
        }
    }
    lines.push(format!(
        "worldgen_demo: leg=empty_rules identical={}",
        identical,
    ));
    if !identical {
        failed.get_or_insert("empty_rules");
    }

    match failed {
        Some(name) => {
            lines.push(format!("worldgen_demo: ok=false failed={name}"));
            (lines, false)
        }
        None => {
            lines.push("worldgen_demo: ok=true".to_string());
            (lines, true)
        }
    }
}
