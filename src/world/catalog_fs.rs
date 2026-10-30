//! Filesystem adapter for the world catalog.
//!
//! Bridges the pure domain in [`crate::world::catalog`] (validation, rows,
//! ordering, duplicates, delete confirmation) to the existing `.mcvx`
//! persistence layer in [`crate::world::persistence`]. This module owns the
//! filesystem and nothing else:
//!
//! - **Listing** scans the saves directory for `*.mcvx` files, derives a
//!   validated world identity from each file stem, reads the modified
//!   timestamp, and probes the file through `load_world_from_path` so
//!   `Loadable` rows are genuinely loadable while corrupt entries degrade
//!   to typed per-row problems instead of disappearing.
//! - **Saving** reuses `save_world_to_path` (atomic sibling-temp + rename)
//!   unchanged; the adapter only resolves the validated target path.
//! - **Loading** goes through `load_world_from_path` unchanged.
//! - **Deleting** accepts only a [`PendingDelete`] produced by the pure
//!   [`crate::world::catalog::DeleteConfirmation::confirm`] call — the
//!   adapter never decides on its own whether a delete may proceed.
//!
//! All failures are typed, user-displayable outcomes; no path in this
//! module panics on filesystem state.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::world::catalog::{
    self, CatalogRow, PendingDelete, RowProblem, RowStatus, WorldNameError,
};
use crate::world::persistence::{
    load_world_from_path, save_world_to_path, PersistenceError, WorldSaveData, DEFAULT_SAVE_PATH,
};

/// Saves directory derived from the existing default save path
/// (`saves/default.mcvx` -> `saves`), so persistence stays the single
/// source of truth for the on-disk layout.
pub fn default_saves_dir() -> PathBuf {
    Path::new(DEFAULT_SAVE_PATH)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Filesystem-backed world catalog over one saves directory.
#[derive(Clone, Debug)]
pub struct WorldCatalogFs {
    saves_dir: PathBuf,
}

/// Failure while resolving a save path for a world name.
///
/// Not `Clone`/`PartialEq`: the wrapped filesystem errors are neither;
/// tests match on variants and error kinds instead.
#[derive(Debug)]
pub enum CatalogSaveError {
    /// The proposed world name is invalid.
    InvalidName(WorldNameError),
    /// The save itself failed (existing persistence semantics, including
    /// its atomic sibling-temp + rename behavior).
    Persistence(PersistenceError),
}

impl fmt::Display for CatalogSaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogSaveError::InvalidName(e) => write!(f, "{e}"),
            CatalogSaveError::Persistence(e) => write!(f, "Could not save the world: {e}"),
        }
    }
}

impl std::error::Error for CatalogSaveError {}

/// Failure while loading a catalog row.
///
/// Not `Clone`/`PartialEq`: the wrapped persistence errors are neither;
/// tests match on variants and error kinds instead.
#[derive(Debug)]
pub enum CatalogLoadError {
    /// The row is corrupt and cannot be loaded at all.
    Corrupt { name: String, problem: RowProblem },
    /// The row looked loadable but the load failed (e.g. the file vanished
    /// between listing and loading, or its body is malformed).
    Load {
        name: String,
        source: PersistenceError,
    },
}

impl fmt::Display for CatalogLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogLoadError::Corrupt { name, problem } => write!(
                f,
                "World \"{name}\" cannot be loaded ({}).",
                match problem {
                    RowProblem::InvalidName => "invalid world name",
                    RowProblem::Unreadable => "file is unreadable",
                    RowProblem::InvalidFormat => "file is not a valid save",
                }
            ),
            CatalogLoadError::Load { name, source } => {
                write!(f, "Could not load world \"{name}\": {source}")
            }
        }
    }
}

impl std::error::Error for CatalogLoadError {}

/// Failure while deleting a confirmed catalog entry.
///
/// Not `Clone`/`PartialEq`: `io::Error` is neither; tests match on
/// variants and error kinds instead.
#[derive(Debug)]
pub enum CatalogDeleteError {
    /// The confirmed file name is not a single safe `*.mcvx` component of
    /// the saves directory. Refused outright; nothing is deleted.
    UnsafeFileName(String),
    /// The filesystem refused the deletion.
    Io(io::Error),
}

impl fmt::Display for CatalogDeleteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogDeleteError::UnsafeFileName(name) => write!(
                f,
                "Refusing to delete \"{name}\": not a safe world save file."
            ),
            CatalogDeleteError::Io(e) => write!(f, "Could not delete the world save: {e}"),
        }
    }
}

impl std::error::Error for CatalogDeleteError {}

/// Classify a persistence failure observed while probing a row:
/// truncated content is a format problem; other I/O failures mean the
/// file could not be read.
fn row_problem_for(error: &PersistenceError) -> RowProblem {
    match error {
        PersistenceError::Io(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
            RowProblem::InvalidFormat
        }
        PersistenceError::Io(_) => RowProblem::Unreadable,
        _ => RowProblem::InvalidFormat,
    }
}

/// Is this path a regular file with the exact `mcvx` extension?
/// (Skips `*.mcvx.tmp` atomic-rename leftovers and unrelated files.)
fn is_world_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .map(|ext| ext == catalog::WORLD_FILE_EXTENSION)
            .unwrap_or(false)
}

/// Modified timestamp in seconds since the Unix epoch, if readable.
fn modified_secs(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Build one catalog row from a `*.mcvx` path.
///
/// Probing loads the file through `load_world_from_path` so a `Loadable`
/// row is guaranteed to actually load. Saves are small header + chunk-edit
/// files, so this full-read probe at menu-open time is cheap; the load
/// path itself reads the file again.
fn build_row(path: &Path) -> CatalogRow {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let modified_secs = modified_secs(path);

    match catalog::validate_world_name(&stem) {
        Ok(name) => match load_world_from_path(path) {
            Ok(_) => CatalogRow::loadable(name, file_name, modified_secs),
            Err(error) => {
                CatalogRow::corrupt(name, file_name, modified_secs, row_problem_for(&error))
            }
        },
        // Invalid stems degrade, not disappear: keep the raw stem so the
        // row can still be displayed and explicitly deleted.
        Err(_) => CatalogRow::corrupt(stem, file_name, modified_secs, RowProblem::InvalidName),
    }
}

/// Is `file_name` a single safe `*.mcvx` component of the saves directory?
fn is_safe_world_file_name(file_name: &str) -> bool {
    if file_name.is_empty()
        || file_name.contains('/')
        || file_name.contains('\\')
        || file_name.contains("..")
    {
        return false;
    }
    let path = Path::new(file_name);
    path.components().count() == 1
        && path
            .extension()
            .map(|ext| ext == catalog::WORLD_FILE_EXTENSION)
            .unwrap_or(false)
}

impl WorldCatalogFs {
    pub fn new(saves_dir: impl Into<PathBuf>) -> Self {
        Self {
            saves_dir: saves_dir.into(),
        }
    }

    /// Catalog over the default saves directory (`saves/`).
    pub fn for_default_saves_dir() -> Self {
        Self::new(default_saves_dir())
    }

    pub fn saves_dir(&self) -> &Path {
        &self.saves_dir
    }

    /// Create the saves directory (and parents) if it does not exist yet.
    pub fn ensure_dir(&self) -> io::Result<()> {
        fs::create_dir_all(&self.saves_dir)
    }

    /// List all world-save rows, newest-first.
    ///
    /// A missing or unlistable saves directory behaves as an empty
    /// catalog (fresh installs show an empty load menu instead of an
    /// error). Per-file problems are represented as corrupt rows rather
    /// than whole-list failures.
    pub fn list_rows(&self) -> Vec<CatalogRow> {
        let mut rows = Vec::new();
        // Transient rename races with save_world_to_path's atomic swap are
        // tolerated per entry (flatten skips dropped iterators); the next
        // listing observes a consistent state.
        let Ok(entries) = fs::read_dir(&self.saves_dir) else {
            return rows;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if is_world_file(&path) {
                rows.push(build_row(&path));
            }
        }
        catalog::sort_rows_newest_first(&mut rows);
        rows
    }

    /// Validated save path for a world named `name`
    /// (`<saves_dir>/<Name>.mcvx`). Pure name validation, no duplicate
    /// check here: callers combine this with `catalog::is_duplicate_name`.
    pub fn world_path(&self, name: &str) -> Result<PathBuf, WorldNameError> {
        let validated = catalog::validate_world_name(name)?;
        Ok(self.saves_dir.join(catalog::world_file_name(&validated)))
    }

    /// Save `data` as the world named `name`, reusing the existing atomic
    /// `save_world_to_path` (sibling-temp + rename) unchanged. Returns the
    /// path written.
    pub fn save_world(
        &self,
        name: &str,
        data: &WorldSaveData,
    ) -> Result<PathBuf, CatalogSaveError> {
        let path = self
            .world_path(name)
            .map_err(CatalogSaveError::InvalidName)?;
        save_world_to_path(&path, data).map_err(CatalogSaveError::Persistence)?;
        Ok(path)
    }

    /// Load a `Loadable` catalog row through the existing
    /// `load_world_from_path`. Corrupt rows fail before touching the
    /// filesystem; load failures (vanished file, malformed body) are
    /// surfaced with the world name for display.
    pub fn load_row(&self, row: &CatalogRow) -> Result<WorldSaveData, CatalogLoadError> {
        match row.status {
            RowStatus::Loadable => load_world_from_path(&self.saves_dir.join(&row.file_name))
                .map_err(|source| CatalogLoadError::Load {
                    name: row.name.clone(),
                    source,
                }),
            RowStatus::Corrupt(problem) => Err(CatalogLoadError::Corrupt {
                name: row.name.clone(),
                problem,
            }),
        }
    }

    /// Delete the world save named by a **confirmed** delete request.
    ///
    /// The decision to delete is owned by the pure
    /// [`catalog::DeleteConfirmation`] state machine: this method accepts
    /// only the [`PendingDelete`] that `confirm()` returned, verifies the
    /// file name is a single safe `*.mcvx` component of the saves
    /// directory, and only then removes the file. Deleting an entry that
    /// already vanished is a success (idempotent): the observable outcome
    /// — the world save is gone — is already true.
    pub fn delete_confirmed(&self, pending: &PendingDelete) -> Result<(), CatalogDeleteError> {
        if !is_safe_world_file_name(&pending.file_name) {
            return Err(CatalogDeleteError::UnsafeFileName(
                pending.file_name.clone(),
            ));
        }
        let path = self.saves_dir.join(&pending.file_name);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(CatalogDeleteError::Io(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::catalog::{
        ConfirmOutcome, DeleteConfirmation, RowStatus, WORLD_FILE_EXTENSION,
    };
    use crate::world::chunk::ChunkPos;
    use crate::world::persistence::SavedPlayerState;
    use std::thread::sleep;
    use std::time::{Duration, SystemTime};

    fn sample_save(seed: u32) -> WorldSaveData {
        let mut edits = crate::world::persistence::ChunkEditMap::default();
        let mut chunk = rustc_hash::FxHashMap::default();
        chunk.insert(10u16, crate::world::block::BlockId::Brick);
        edits.insert(ChunkPos::new(0, 0, 0), chunk);
        WorldSaveData {
            seed,
            time_of_day: crate::world::daycycle::DEFAULT_TIME_OF_DAY,
            day_index: 0,
            season_length_days: crate::world::season::DEFAULT_SEASON_LENGTH_DAYS,
            player: SavedPlayerState {
                position: [1.0, 64.0, -2.0],
                yaw: 0.5,
                pitch: -0.25,
                fly_mode: true,
                selected_slot: 2,
                render_distance: 16,
                inventory: crate::player::inventory::Inventory::empty(),
                player_grid: crate::player::inventory::CraftingGrid::new(
                    crate::player::inventory::GridKind::Player,
                ),
                table_grid: crate::player::inventory::CraftingGrid::new(
                    crate::player::inventory::GridKind::Table,
                ),
            },
            chunk_edits: edits,
        }
    }

    fn row_for<'a>(rows: &'a [CatalogRow], name: &str) -> &'a CatalogRow {
        rows.iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("row {name} should be listed"))
    }

    #[test]
    fn missing_or_empty_saves_dir_lists_no_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Not yet created.
        let missing = WorldCatalogFs::new(dir.path().join("saves"));
        assert!(missing.list_rows().is_empty());

        // Created but empty.
        missing.ensure_dir().expect("ensure_dir");
        assert!(missing.list_rows().is_empty());
        assert!(missing.saves_dir().is_dir());
    }

    #[test]
    fn nested_background_save_is_not_listed_as_a_world() {
        // M012: the background lives at saves/background/background.mcvx.
        // The catalog scans only top-level saves/*.mcvx entries, so the
        // backdrop can never be loaded or deleted as a playable world.
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog = WorldCatalogFs::new(dir.path().join("saves"));
        catalog.ensure_dir().expect("ensure_dir");
        let background = catalog.saves_dir().join("background/background.mcvx");
        save_world_to_path(&background, &sample_save(7)).expect("write background");
        catalog
            .save_world("Playable", &sample_save(9))
            .expect("write playable world");

        let rows = catalog.list_rows();
        assert_eq!(rows.len(), 1, "only top-level *.mcvx files are worlds");
        assert_eq!(rows[0].name, "Playable");
        assert!(
            background.exists(),
            "the nested background file is untouched"
        );
    }

    /// `modified_secs` has whole-second granularity, so two writes inside
    /// the same wall-clock second tie and fall back to the name tie-break.
    /// Sleeping past the next second boundary makes the newest-first
    /// ordering assertion deterministic.
    fn sleep_past_second_boundary() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch");
        let millis_into_second = u64::from(now.subsec_millis());
        sleep(Duration::from_millis(1050 - millis_into_second.min(999)));
    }

    #[test]
    fn saved_worlds_list_newest_first_and_roundtrip_through_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog_fs = WorldCatalogFs::new(dir.path());

        let first = catalog_fs
            .save_world("alpha", &sample_save(111))
            .expect("save alpha");
        assert_eq!(first, dir.path().join("alpha.mcvx"));
        sleep_past_second_boundary();
        catalog_fs
            .save_world("omega", &sample_save(222))
            .expect("save omega");

        let rows = catalog_fs.list_rows();
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["omega", "alpha"]); // newest first
        assert!(rows[0].modified_secs.unwrap() > 0);
        assert!(rows[0].modified_secs.unwrap() > rows[1].modified_secs.unwrap());

        // Loadable rows genuinely load with the saved data.
        let omega = row_for(&rows, "omega");
        assert!(omega.is_loadable());
        let loaded = catalog_fs.load_row(omega).expect("load omega");
        assert_eq!(loaded, sample_save(222));
    }

    #[test]
    fn default_save_file_remains_discoverable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog_fs = WorldCatalogFs::new(dir.path());

        // Write the file exactly where the legacy default save lives
        // (saves/default.mcvx equivalent, at this dir's root).
        catalog_fs
            .save_world("default", &sample_save(7))
            .expect("save default");

        let rows = catalog_fs.list_rows();
        let default = row_for(&rows, "default");
        assert_eq!(default.file_name, format!("default.{WORLD_FILE_EXTENSION}"));
        assert!(default.is_loadable());
        assert_eq!(catalog_fs.load_row(default).expect("load").seed, 7);
    }

    #[test]
    fn non_world_files_and_tmp_siblings_are_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog_fs = WorldCatalogFs::new(dir.path());

        catalog_fs
            .save_world("real", &sample_save(1))
            .expect("save");
        fs::write(dir.path().join("notes.txt"), b"not a save").expect("write txt");
        // Leftover from an interrupted atomic rename must not be listed.
        fs::write(dir.path().join("leftover.mcvx.tmp"), b"garbage").expect("write tmp");
        // A directory wearing the extension is not a save file.
        fs::create_dir(dir.path().join("folder.mcvx")).expect("mkdir");

        let rows = catalog_fs.list_rows();
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["real"]);
    }

    #[test]
    fn malformed_files_degrade_to_typed_corrupt_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog_fs = WorldCatalogFs::new(dir.path());

        // Bad magic.
        fs::write(dir.path().join("badmagic.mcvx"), b"NOPE!1234").expect("write");
        // Truncated below the header.
        fs::write(dir.path().join("short.mcvx"), b"MCV").expect("write");
        // Valid magic but unsupported version.
        let mut bad_version = Vec::new();
        bad_version.extend_from_slice(crate::world::persistence::SAVE_MAGIC);
        bad_version.extend_from_slice(&99u32.to_le_bytes());
        fs::write(dir.path().join("oldver.mcvx"), &bad_version).expect("write");
        // Filename stem that is not a valid world name.
        fs::write(dir.path().join("bad%name.mcvx"), b"whatever").expect("write");

        let rows = catalog_fs.list_rows();
        assert_eq!(rows.len(), 4);

        let badmagic = row_for(&rows, "badmagic");
        assert_eq!(
            badmagic.status,
            RowStatus::Corrupt(RowProblem::InvalidFormat)
        );
        let short = row_for(&rows, "short");
        assert_eq!(short.status, RowStatus::Corrupt(RowProblem::InvalidFormat));
        let oldver = row_for(&rows, "oldver");
        assert_eq!(oldver.status, RowStatus::Corrupt(RowProblem::InvalidFormat));
        let bad_name = row_for(&rows, "bad%name");
        assert_eq!(bad_name.status, RowStatus::Corrupt(RowProblem::InvalidName));
        // The raw stem is kept so the row can still be displayed and deleted.
        assert_eq!(bad_name.file_name, "bad%name.mcvx");

        // Corrupt rows refuse loading with a user-displayable error.
        for row in [badmagic, short, oldver, bad_name] {
            match catalog_fs.load_row(row) {
                Err(CatalogLoadError::Corrupt { .. }) => {}
                other => panic!("expected Corrupt error, got {other:?}"),
            }
        }
    }

    #[cfg(unix)]
    fn running_as_root() -> bool {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status.lines().find_map(|line| {
                    let rest = line.strip_prefix("Uid:")?;
                    rest.split_whitespace().nth(1).map(|euid| euid == "0")
                })
            })
            .unwrap_or(false)
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_files_list_as_unreadable_rows() {
        if running_as_root() {
            // Root bypasses permission bits; the scenario cannot be staged.
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog_fs = WorldCatalogFs::new(dir.path());
        catalog_fs
            .save_world("secret", &sample_save(3))
            .expect("save");

        use std::os::unix::fs::PermissionsExt;
        let path = dir.path().join("secret.mcvx");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("chmod");

        let rows = catalog_fs.list_rows();
        let secret = row_for(&rows, "secret");
        assert_eq!(secret.status, RowStatus::Corrupt(RowProblem::Unreadable));

        // Restore so TempDir cleanup is unhindered.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
    }

    #[test]
    fn vanished_files_surface_as_structured_load_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog_fs = WorldCatalogFs::new(dir.path());
        catalog_fs
            .save_world("ghost", &sample_save(5))
            .expect("save");

        let rows = catalog_fs.list_rows();
        let ghost = row_for(&rows, "ghost").clone();
        // The file disappears between listing and loading.
        fs::remove_file(dir.path().join("ghost.mcvx")).expect("remove");

        match catalog_fs.load_row(&ghost) {
            Err(CatalogLoadError::Load { name, source }) => {
                assert_eq!(name, "ghost");
                match source {
                    PersistenceError::Io(e) => {
                        assert_eq!(e.kind(), io::ErrorKind::NotFound)
                    }
                    other => panic!("expected NotFound io error, got {other:?}"),
                }
            }
            other => panic!("expected Load error, got {other:?}"),
        }
    }

    #[test]
    fn delete_requires_pure_confirmation_and_only_removes_the_named_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog_fs = WorldCatalogFs::new(dir.path());
        catalog_fs
            .save_world("doomed", &sample_save(1))
            .expect("save");
        catalog_fs
            .save_world("spared", &sample_save(2))
            .expect("save");

        let mut confirm = DeleteConfirmation::new();
        let doomed_row = row_for(&catalog_fs.list_rows(), "doomed").clone();

        // Cancelled request never deletes: confirm after cancel has nothing
        // pending, and the adapter is not even called.
        confirm.request(&doomed_row);
        confirm.cancel();
        assert_eq!(confirm.confirm(), ConfirmOutcome::NothingPending);
        assert!(dir.path().join("doomed.mcvx").is_file());

        // Confirmed request deletes exactly the named file.
        confirm.request(&doomed_row);
        let ConfirmOutcome::Confirmed(pending) = confirm.confirm() else {
            panic!("expected a confirmed delete");
        };
        assert_eq!(pending.file_name, "doomed.mcvx");
        catalog_fs.delete_confirmed(&pending).expect("delete");

        assert!(!dir.path().join("doomed.mcvx").exists());
        assert!(dir.path().join("spared.mcvx").is_file());
        let rows = catalog_fs.list_rows();
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["spared"]);

        // Re-deleting the vanished entry is idempotent, not an error.
        catalog_fs.delete_confirmed(&pending).expect("idempotent");
    }

    #[test]
    fn delete_refuses_unsafe_and_non_world_file_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog_fs = WorldCatalogFs::new(dir.path());
        catalog_fs
            .save_world("real", &sample_save(1))
            .expect("save");
        fs::write(dir.path().join("notes.txt"), b"keep me").expect("write txt");

        for bad in ["../escape.mcvx", "notes.txt", "sub/dir.mcvx", "..", ""] {
            let pending = PendingDelete {
                name: bad.to_string(),
                file_name: bad.to_string(),
            };
            match catalog_fs.delete_confirmed(&pending) {
                Err(CatalogDeleteError::UnsafeFileName(_)) => {}
                other => panic!("expected UnsafeFileName for {bad:?}, got {other:?}"),
            }
        }

        // Nothing outside the confirmed world file was touched.
        assert!(dir.path().join("notes.txt").is_file());
        assert!(dir.path().join("real.mcvx").is_file());
        // No file escaped the saves directory.
        assert!(!dir.path().parent().unwrap().join("escape.mcvx").exists());
    }

    #[test]
    fn save_world_validates_names_and_supports_duplicate_detection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog_fs = WorldCatalogFs::new(dir.path());

        // Invalid names are typed, user-displayable failures.
        for bad in ["", "   ", "bad/name", "colon:name", &"x".repeat(33)] {
            match catalog_fs.save_world(bad, &sample_save(1)) {
                Err(CatalogSaveError::InvalidName(_)) => {}
                other => panic!("expected InvalidName for {bad:?}, got {other:?}"),
            }
        }

        let path = catalog_fs
            .save_world("Overworld", &sample_save(9))
            .expect("save");
        assert_eq!(path, dir.path().join("Overworld.mcvx"));

        let rows = catalog_fs.list_rows();
        assert!(catalog::is_duplicate_name(" overworld ", &rows));
        assert!(catalog::is_duplicate_name("OVERWORLD", &rows));
        assert!(!catalog::is_duplicate_name("Other", &rows));

        // Validated path resolution trims and joins the saves directory.
        let resolved = catalog_fs.world_path("  My World  ").expect("valid");
        assert_eq!(resolved, dir.path().join("My World.mcvx"));
    }

    #[test]
    fn default_saves_dir_derives_from_persistence_default_save_path() {
        assert_eq!(default_saves_dir(), PathBuf::from("saves"));
        assert_eq!(
            WorldCatalogFs::for_default_saves_dir().saves_dir(),
            Path::new("saves")
        );
    }
}
