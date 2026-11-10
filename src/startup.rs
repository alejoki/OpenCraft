//! Startup session construction: the single load-or-fresh path shared by
//! main() and the headless restart digest.

use std::path::Path;
use std::sync::Arc;

use crate::config::{ConfigRuntime, RulesBundle, SaveConfigMeta};
use crate::content::registry::{build_registry, ContentLoadReport, PackIdentity};
use crate::content::{ContentRegistry, PackContentSpec};
use crate::demo::combat::COMBAT_DUSK_TICK;
use crate::demo::swim::{arm_swim_route, swim_route};
use crate::demo::watch::demo_owns_world_witness;
use crate::pack::spec::PackSpec;
use crate::pack::{self, LoadedPack};
use crate::player::survival::GameMode;
use crate::settings::SettingsWarning;
use crate::world::daycycle::GameClock;
use crate::world::persistence::{load_world_from_path_with, CliConfig, DEFAULT_TIME_SCALE};
use crate::world::session::WorldSession;
use crate::world::terrain::DEFAULT_WORLD_SEED;
use crate::world::village::VillageCatalog;

/// Load the persisted ordered pack stack at startup, returning the merged
/// presentation plus the per-pack specs and their per-file fallback warnings.
/// The caller merges the warnings into `startup_warnings` (the S03 routing
/// contract: warnings are data shown in the menu, never logs). An empty stack
/// is the built-in Default pack: no presentation, no specs, no warnings. A
/// missing stack entry stays in the stack and only produces a warning, so
/// restoring the folder restores the pack without re-adding it.
pub fn load_startup_stack(
    packs_root: &Path,
    stack: &[String],
) -> (Option<LoadedPack>, Vec<PackSpec>, Vec<SettingsWarning>) {
    if stack.is_empty() {
        return (None, Vec::new(), Vec::new());
    }
    let (loaded, specs, warnings) = pack::load_stack(packs_root, stack);
    (Some(loaded), specs, warnings)
}

/// Build the session's immutable content registry from the ordered pack specs.
/// The built-in table is the base; later packs win per definition. The caller
/// freezes the result in an `Arc` and never replaces it mid-session (MEM512).
pub fn build_session_registry(specs: &[PackSpec]) -> (Arc<ContentRegistry>, ContentLoadReport) {
    let packs: Vec<(PackIdentity, PackContentSpec)> = specs
        .iter()
        .map(|spec| {
            (
                PackIdentity {
                    folder: spec.folder.clone(),
                    version: spec.version.clone(),
                },
                spec.content.clone(),
            )
        })
        .collect();
    let (registry, report) = build_registry(ContentRegistry::builtin(), &packs);
    (Arc::new(registry), report)
}

/// Re-read the authored village/prefab catalog from `root`.
///
/// Wraps [`VillageCatalog::load_sorted`] so `attempt_create` can pick up
/// content edits without an app restart and tests can exercise the reload in
/// isolation. Deliberately uncached: each call re-walks the tree, which is the
/// point of the reload seam.
pub fn reload_village_catalog(root: &Path) -> (Arc<VillageCatalog>, Vec<String>) {
    let (catalog, diagnostics) = VillageCatalog::load_sorted(root);
    (Arc::new(catalog), diagnostics)
}

/// The startup world session, mirroring main()'s pre-S03 load-or-fresh
/// block EXACTLY (byte-identical legacy behavior — including the
/// default.mcvx launch and every CLI-flag direct launch — now testable
/// without an event loop): when the cli save path exists and the launch
/// did not force a new world, load it and rebuild the session via
/// `WorldSession::session_from_save` (player state, seed, render
/// distance, chunk edits) with `save_path = cli.save_path`; otherwise a
/// fresh `WorldSession::new_session` at
/// `cli.seed_override.unwrap_or(DEFAULT_WORLD_SEED)` with the same save
/// path.
///
/// Built-in wrapper: production builds `StartupContent` first and calls
/// [`startup_session_with`] with its base registry; tests and demos use this.
pub fn startup_session(cli: &CliConfig) -> WorldSession {
    startup_session_with(
        cli,
        &ContentRegistry::builtin_arc(),
        &ConfigRuntime::defaults(),
    )
}

/// The startup world session resolved against `registry`.
///
/// The load branch keeps the placeholder-augmented registry and the one-shot
/// changed-stack notice; the fresh branch freezes the supplied registry on the
/// new session. [`startup_session`] is the built-in wrapper every existing
/// test and demo launch uses. `config` is the effective gameplay tuning loaded
/// from `config/` (M014/S01); both branches record its rules and its validated
/// save meta (M014/S04) so the next save and the one-shot notice agree.
pub fn startup_session_with(
    cli: &CliConfig,
    registry: &Arc<ContentRegistry>,
    config: &ConfigRuntime,
) -> WorldSession {
    let rules: RulesBundle = config.rules();
    let meta: SaveConfigMeta = config.meta();
    if !cli.force_new_world && cli.save_path.exists() {
        if let Ok(loaded) = load_world_from_path_with(&cli.save_path, registry.as_ref()) {
            let mut session = WorldSession::from_loaded_world(loaded, cli.save_path.clone());
            // A bounded accelerated watch that reloads a save must keep the
            // same pinned witness as its fresh leg: no ambient records may
            // appear in the next saved tail. The scripted demos are
            // deliberately excluded here: a loaded `--demo-combat` leg is a
            // one-call Restore that re-saves nothing, and T03 pinned that only
            // the fresh scripted branch closes the gate.
            if cli.time_scale > DEFAULT_TIME_SCALE {
                session.set_ambient_spawning(false);
            }
            if cli.demo_combat {
                // The pinned quartet sits 3.5 blocks from the player, inside
                // the 16-block aggro, so hostile AI must stay off until the
                // dedicated `Hostile` stage reopens it. The load branch is a
                // one-call Restore leg, but the gate is ephemeral state the
                // session must still start closed for the scripted run.
                session.set_hostile_ai_enabled(false);
            }
            session.apply_config(&rules, meta);
            return session;
        }
    }
    let seed = cli.seed_override.unwrap_or(DEFAULT_WORLD_SEED);
    // The scripted combat seam (`--demo-combat`, M009/S01-T05) pins a fresh
    // world to Survival at dusk so the mob path is proven with walking mobs in
    // front of a non-flying player. It overrides `cli.mode` on the fresh
    // branch only: the load branch above returns first, so a saved world is
    // never re-moded, re-clocked, or re-seeded by the flag.
    let mode = if cli.demo_combat {
        GameMode::Survival
    } else {
        cli.mode
    };
    // Fresh worlds start at the default season length; the New Game row and
    // the `--season-length` CLI seam override it on this branch only. A load
    // branch above never reaches here, so a persisted calendar always wins
    // over a launch flag. The mode follows the same rule: `--creative` /
    // `--survival` pick the fresh world's persisted mode, while a loaded save
    // restores its own stored mode and ignores the flag.
    let mut session = WorldSession::new_session_with_mode_and_registry(
        seed,
        cli.save_path.clone(),
        cli.season_length,
        mode,
        Arc::clone(registry),
    );
    if cli.demo_combat {
        session.clock = GameClock::from_time(COMBAT_DUSK_TICK);
    }
    // Ephemeral population gate (M009/S05-T03): the pinned `combat_demo:`
    // digest and every bounded demo's v9 save tail must describe only what that
    // demo placed, so ambient spawning and daylight burning stay off for the
    // whole run. Production sessions never touch this switch.
    if demo_owns_world_witness(cli) {
        session.set_ambient_spawning(false);
    }
    // The pinned quartet sits 3.5 blocks from the player, inside the
    // 16-block aggro, so hostile AI must stay off until the dedicated
    // `Hostile` stage reopens it. Guarded on `demo_combat` alone: the other
    // scripted demos and the accelerated watch must keep their behaviour.
    if cli.demo_combat {
        session.set_hostile_ai_enabled(false);
    }
    // Scripted ocean-crossing seam (S08-T02): only a fresh world is teleported
    // onto the pinned route. The load branch above returns before this, so a
    // resumed save is never moved — its own position is the contract there.
    if cli.demo_swim {
        if let Some(route) = swim_route(seed) {
            arm_swim_route(&mut session, route);
        }
    }
    session.apply_config(&rules, meta);
    session
}
