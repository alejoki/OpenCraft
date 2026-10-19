//! GPU-independent domain logic for named worlds: seed resolution, world-name
//! validation, catalog rows, ordering, duplicate detection, and explicit
//! delete confirmation.
//!
//! Like `src/ui/lifecycle.rs`, this module is pure: it owns no filesystem,
//! renderer, or app state. All values and metadata (file names, modified
//! timestamps, row status) are injected by the caller, so every behavior can
//! be exercised without a real app or renderer. The filesystem adapter
//! (built in a later task) is responsible for actually reading directories,
//! loading through `world::persistence`, and deleting files — and must
//! delete only after the pure confirmation decision here is confirmed.

/// Maximum accepted world-name length (in `char`s), matching typical UI
/// text-entry bounds and keeping derived file names short.
pub const MAX_WORLD_NAME_LEN: usize = 32;

/// File extension for saved worlds (kept in sync with `world::persistence`).
pub const WORLD_FILE_EXTENSION: &str = "mcvx";

// ---------------------------------------------------------------------------
// Seed resolution
// ---------------------------------------------------------------------------

/// The result of resolving free-text seed input.
///
/// `Random` means "no explicit seed chosen"; the caller decides how to
/// generate the concrete value (e.g. from the OS RNG), keeping this module
/// deterministic and testable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedSource {
    /// A stable seed derived from non-empty free text.
    Text(u32),
    /// The input was blank; the caller should pick a fresh seed.
    Random,
}

/// Resolve free-text seed input to a seed source.
///
/// Text that is empty or entirely whitespace resolves to
/// [`SeedSource::Random`]. Any other text resolves to a deterministic
/// FNV-1a 32-bit hash of the *trimmed* input, so the same text always
/// produces the same world, distinct texts practically never collide, and
/// trailing whitespace does not change the result.
pub fn resolve_seed(input: &str) -> SeedSource {
    let text = input.trim();
    if text.is_empty() {
        return SeedSource::Random;
    }
    // FNV-1a 32-bit: stable, dependency-free, deterministic across runs
    // (std's SipHash-based hasher offers no cross-run stability guarantee).
    let mut hash: u32 = 0x811C_9DC5;
    for byte in text.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    SeedSource::Text(hash)
}

/// Convenience wrapper for callers that already hold a random fallback: a
/// blank input yields `random_seed`, otherwise the text hash.
pub fn resolve_seed_or(input: &str, random_seed: u32) -> u32 {
    match resolve_seed(input) {
        SeedSource::Text(seed) => seed,
        SeedSource::Random => random_seed,
    }
}

// ---------------------------------------------------------------------------
// World-name validation
// ---------------------------------------------------------------------------

/// Typed validation failure for a proposed world name. These are
/// user-displayable outcomes, not panics: invalid names are an expected
/// input class, not exceptional.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorldNameError {
    /// Name is empty or only whitespace after trimming.
    Empty,
    /// Name (in `char`s) exceeds [`MAX_WORLD_NAME_LEN`].
    TooLong { len: usize },
    /// Character at `index` is not filename-safe.
    ///
    /// Allowed characters: alphanumeric, space, `-`, and `_` (Unicode
    /// alphanumeric via `char::is_alphanumeric`). Everything else — path
    /// separators, `:` `*` `?` `"` `<` `>` `|`, control characters, dots,
    /// and other punctuation — is rejected so the name can be used directly
    /// as a file stem on Windows, macOS, and Linux.
    InvalidCharacter { ch: char, index: usize },
}

impl core::fmt::Display for WorldNameError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WorldNameError::Empty => write!(f, "World name cannot be empty."),
            WorldNameError::TooLong { len } => write!(
                f,
                "World name is too long ({len} characters; max {MAX_WORLD_NAME_LEN})."
            ),
            WorldNameError::InvalidCharacter { ch, index } => {
                write!(
                    f,
                    "Character {ch:?} at position {index} is not allowed in a world name."
                )
            }
        }
    }
}

/// Check whether a single character is filename-safe for a world name.
pub fn is_valid_name_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == ' ' || ch == '-' || ch == '_'
}

/// Validate a proposed world name for use as a file stem.
///
/// Returns the canonical (trimmed) name on success. Whitespace-only input
/// and names longer than [`MAX_WORLD_NAME_LEN`] are rejected, as is any
/// character outside the allowed set above.
pub fn validate_world_name(name: &str) -> Result<String, WorldNameError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(WorldNameError::Empty);
    }
    let len = trimmed.chars().count();
    if len > MAX_WORLD_NAME_LEN {
        return Err(WorldNameError::TooLong { len });
    }
    for (index, ch) in trimmed.char_indices() {
        if !is_valid_name_char(ch) {
            return Err(WorldNameError::InvalidCharacter { ch, index });
        }
    }
    Ok(trimmed.to_owned())
}

/// File stem for a validated world name (the `Name` in `Name.mcvx`).
/// Assumes the name has already been validated; a valid name needs no
/// further escaping.
pub fn world_file_stem(name: &str) -> &str {
    name.trim()
}

/// File name (including extension) for a validated world name.
pub fn world_file_name(name: &str) -> String {
    format!("{}.{}", world_file_stem(name), WORLD_FILE_EXTENSION)
}

// ---------------------------------------------------------------------------
// Catalog rows
// ---------------------------------------------------------------------------

/// Why a catalog row could not be turned into a loadable world identity.
/// Corrupt rows are still listed (degradation, not exclusion): the user can
/// see that the entry exists and choose to delete it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowProblem {
    /// The file stem is not a valid world name.
    InvalidName,
    /// The file could not be read (permissions, locked, vanished).
    Unreadable,
    /// The file was read but rejected by the save loader.
    /// (Magic/version/binary errors are represented by the wrapped
    /// persistence error kind.)
    InvalidFormat,
}

/// Load state of one catalog entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowStatus {
    /// Identity fully derived and loadable.
    Loadable,
    /// The entry is listed but cannot be loaded or loaded-safely identified.
    Corrupt(RowProblem),
}

impl RowStatus {
    pub fn is_loadable(self) -> bool {
        self == RowStatus::Loadable
    }
}

/// One row of the load-game catalog. All metadata is injected by the
/// filesystem adapter: this type only records what was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogRow {
    /// Validated, trimmed world name derived from the file stem. For
    /// [`RowStatus::Corrupt(RowProblem::InvalidName)`] rows this is the raw
    /// stem (best-effort) so the row can still be displayed and deleted.
    pub name: String,
    /// Full file name as found in the saves directory, e.g. `default.mcvx`.
    pub file_name: String,
    /// Modified timestamp in seconds since the Unix epoch, if readable.
    /// `None` sorts last (see [`sort_rows_newest_first`]).
    pub modified_secs: Option<u64>,
    pub status: RowStatus,
}

impl CatalogRow {
    /// Build a `Loadable` row from injected metadata.
    pub fn loadable(name: String, file_name: String, modified_secs: Option<u64>) -> Self {
        Self {
            name,
            file_name,
            modified_secs,
            status: RowStatus::Loadable,
        }
    }

    /// Build a `Corrupt` row from injected metadata.
    pub fn corrupt(
        name: String,
        file_name: String,
        modified_secs: Option<u64>,
        problem: RowProblem,
    ) -> Self {
        Self {
            name,
            file_name,
            modified_secs,
            status: RowStatus::Corrupt(problem),
        }
    }

    pub fn is_loadable(&self) -> bool {
        self.status.is_loadable()
    }
}

/// Sort catalog rows newest-first.
///
/// Ordering rules, in priority order:
/// 1. Higher `modified_secs` first (`None` timestamps sort last, after all
///    known timestamps).
/// 2. Equal or both-missing timestamps: ascending `name` comparison, so the
///    result is deterministic and stable regardless of the directory order
///    the adapter observed.
pub fn sort_rows_newest_first(rows: &mut [CatalogRow]) {
    rows.sort_by(|a, b| {
        let ts = match (a.modified_secs, b.modified_secs) {
            (Some(x), Some(y)) => y.cmp(&x),
            (Some(_), None) => core::cmp::Ordering::Less,
            (None, Some(_)) => core::cmp::Ordering::Greater,
            (None, None) => core::cmp::Ordering::Equal,
        };
        ts.then_with(|| a.name.cmp(&b.name))
    });
}

/// Find the first row whose name matches `name` case-insensitively.
///
/// Case-insensitive comparison prevents near-duplicate saves from slipping
/// past on platforms with case-insensitive filesystems (macOS, Windows).
pub fn find_duplicate<'a>(
    name: &str,
    rows: impl IntoIterator<Item = &'a CatalogRow>,
) -> Option<&'a CatalogRow> {
    let needle = name.trim().to_lowercase();
    rows.into_iter()
        .find(|row| row.name.to_lowercase() == needle)
}

/// Check whether a proposed world name collides with any existing row.
pub fn is_duplicate_name(name: &str, rows: &[CatalogRow]) -> bool {
    find_duplicate(name, rows.iter()).is_some()
}

// ---------------------------------------------------------------------------
// Delete confirmation
// ---------------------------------------------------------------------------

/// One catalog row pending explicit delete confirmation. Stored by value so
/// the UI can render the prompt without borrowing the catalog list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingDelete {
    /// Display name for the confirmation prompt.
    pub name: String,
    /// File name the adapter should delete once confirmed.
    pub file_name: String,
}

/// Explicit delete-confirmation state machine.
///
/// Deleting is a destructive action: the adapter must call [`request`],
/// then act only on the `Some` return of [`confirm`]. [`cancel`] (or any
/// new request) always returns the machine to the inactive state, so a
/// delete can never fire from a stale confirmation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeleteConfirmation {
    pending: Option<PendingDelete>,
}

/// The explicit outcome of a confirmation click.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfirmOutcome {
    /// Confirmed: delete the file named inside.
    Confirmed(PendingDelete),
    /// Confirmed with nothing pending — nothing to do.
    NothingPending,
}

impl DeleteConfirmation {
    pub const fn new() -> Self {
        Self { pending: None }
    }

    /// Is a delete currently awaiting confirmation?
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// The row currently awaiting confirmation, if any.
    pub fn pending(&self) -> Option<&PendingDelete> {
        self.pending.as_ref()
    }

    /// Ask for confirmation to delete `row`. Replaces any previous pending
    /// request so a second click elsewhere cannot confirm the first row.
    pub fn request(&mut self, row: &CatalogRow) {
        self.pending = Some(PendingDelete {
            name: row.name.clone(),
            file_name: row.file_name.clone(),
        });
    }

    /// User confirmed. Returns the pending delete (consuming the request) or
    /// [`ConfirmOutcome::NothingPending`] when no request was active.
    pub fn confirm(&mut self) -> ConfirmOutcome {
        match self.pending.take() {
            Some(pending) => ConfirmOutcome::Confirmed(pending),
            None => ConfirmOutcome::NothingPending,
        }
    }

    /// User cancelled (or navigated away). Clears any pending request.
    pub fn cancel(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- seed resolution ---------------------------------------------------

    #[test]
    fn seed_is_deterministic_for_the_same_text() {
        assert_eq!(resolve_seed("hello"), resolve_seed("hello"));
        assert_eq!(resolve_seed(""), resolve_seed(""));
        assert_eq!(resolve_seed("  "), resolve_seed("\t\n "));
        // Trimming means surrounding whitespace does not change the hash.
        assert_eq!(resolve_seed("hello"), resolve_seed("  hello  "));
    }

    #[test]
    fn seed_distinct_inputs_hash_differently() {
        let a = resolve_seed("alpha");
        let b = resolve_seed("beta");
        let c = resolve_seed("alpha2");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert!(matches!(a, SeedSource::Text(_)));
    }

    #[test]
    fn blank_seed_resolves_to_random() {
        assert_eq!(resolve_seed(""), SeedSource::Random);
        assert_eq!(resolve_seed("   "), SeedSource::Random);
        assert_eq!(resolve_seed_or("", 777), 777);
        assert_eq!(resolve_seed_or(" \t ", 888), 888);
        // Non-blank still wins over the fallback.
        assert_eq!(
            resolve_seed_or("known", 777),
            match resolve_seed("known") {
                SeedSource::Text(s) => s,
                SeedSource::Random => unreachable!(),
            }
        );
    }

    // --- world-name validation --------------------------------------------

    #[test]
    fn valid_world_names_pass_and_are_trimmed() {
        assert_eq!(validate_world_name("Overworld"), Ok("Overworld".into()));
        assert_eq!(validate_world_name("  My World  "), Ok("My World".into()));
        assert_eq!(
            validate_world_name("deep_dark-42"),
            Ok("deep_dark-42".into())
        );
        assert_eq!(validate_world_name("ä"), Ok("ä".into())); // Unicode alphanumeric
                                                              // Max length boundary is inclusive.
        let max_name = "a".repeat(MAX_WORLD_NAME_LEN);
        assert_eq!(validate_world_name(&max_name), Ok(max_name.clone()));
        assert_eq!(world_file_name(&max_name), format!("{max_name}.mcvx"));
        assert_eq!(world_file_name("default"), "default.mcvx");
    }

    #[test]
    fn invalid_world_names_are_typed_errors() {
        assert_eq!(validate_world_name(""), Err(WorldNameError::Empty));
        assert_eq!(validate_world_name("   "), Err(WorldNameError::Empty));

        let too_long = "a".repeat(MAX_WORLD_NAME_LEN + 1);
        assert_eq!(
            validate_world_name(&too_long),
            Err(WorldNameError::TooLong {
                len: MAX_WORLD_NAME_LEN + 1
            })
        );

        // Path separators and Windows-reserved punctuation.
        for bad in ['/', '\\', ':', '*', '?', '"', '<', '>', '|', '.', ';', '%'] {
            assert_eq!(
                validate_world_name(&format!("bad{bad}name")),
                Err(WorldNameError::InvalidCharacter { ch: bad, index: 3 })
            );
        }
        // Control character at its exact index.
        assert_eq!(
            validate_world_name("a\u{7}b"),
            Err(WorldNameError::InvalidCharacter {
                ch: '\u{7}',
                index: 1
            })
        );
        // The error is user-displayable.
        assert!(WorldNameError::InvalidCharacter { ch: '/', index: 3 }
            .to_string()
            .contains('/'));
    }

    // --- ordering ----------------------------------------------------------

    #[test]
    fn rows_sort_newest_first_with_stable_name_ties() {
        let mut rows = vec![
            CatalogRow::loadable("old".into(), "old.mcvx".into(), Some(100)),
            CatalogRow::loadable("new".into(), "new.mcvx".into(), Some(500)),
            CatalogRow::loadable("mid".into(), "mid.mcvx".into(), Some(300)),
        ];
        sort_rows_newest_first(&mut rows);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["new", "mid", "old"]);

        // Equal timestamps: ascending name order regardless of input order.
        let mut rows = vec![
            CatalogRow::loadable("zeta".into(), "zeta.mcvx".into(), Some(10)),
            CatalogRow::loadable("beta".into(), "beta.mcvx".into(), Some(10)),
            CatalogRow::loadable("alpha".into(), "alpha.mcvx".into(), Some(10)),
        ];
        sort_rows_newest_first(&mut rows);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["alpha", "beta", "zeta"]);

        // Corrupt rows degrade, not disappear: still ordered by timestamp.
        let mut rows = vec![
            CatalogRow::corrupt(
                "broken".into(),
                "broken.mcvx".into(),
                Some(900),
                RowProblem::InvalidFormat,
            ),
            CatalogRow::loadable("fine".into(), "fine.mcvx".into(), Some(10)),
        ];
        sort_rows_newest_first(&mut rows);
        assert_eq!(rows[0].name, "broken");
        assert_eq!(
            rows[0].status,
            RowStatus::Corrupt(RowProblem::InvalidFormat)
        );
        assert!(rows[1].is_loadable());
    }

    #[test]
    fn rows_without_timestamps_sort_last() {
        let mut rows = vec![
            CatalogRow::loadable("unknown".into(), "unknown.mcvx".into(), None),
            CatalogRow::loadable("dated".into(), "dated.mcvx".into(), Some(1)),
            CatalogRow::loadable("also-unknown".into(), "u2.mcvx".into(), None),
        ];
        sort_rows_newest_first(&mut rows);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["dated", "also-unknown", "unknown"]);
    }

    // --- duplicate detection -----------------------------------------------

    #[test]
    fn duplicate_detection_is_case_insensitive_and_exact() {
        let rows = vec![
            CatalogRow::loadable("Overworld".into(), "Overworld.mcvx".into(), Some(10)),
            CatalogRow::loadable("Second".into(), "second.mcvx".into(), Some(20)),
        ];
        assert!(is_duplicate_name("Overworld", &rows));
        assert!(is_duplicate_name(" overworld ", &rows)); // trimmed
        assert!(is_duplicate_name("OVERWORLD", &rows)); // case-insensitive
        assert!(!is_duplicate_name("Other", &rows));
        assert!(!is_duplicate_name("Overworld2", &rows)); // not a prefix match

        let hit = find_duplicate("SECOND", rows.iter()).expect("match");
        assert_eq!(hit.file_name, "second.mcvx");
    }

    // --- delete confirmation ------------------------------------------------

    #[test]
    fn delete_requires_explicit_confirm_and_consumes_the_request() {
        let row = CatalogRow::loadable("doomed".into(), "doomed.mcvx".into(), Some(5));
        let mut confirm = DeleteConfirmation::new();
        assert!(!confirm.is_pending());
        assert_eq!(confirm.confirm(), ConfirmOutcome::NothingPending); // stray click

        confirm.request(&row);
        assert!(confirm.is_pending());
        assert_eq!(confirm.pending().map(|p| p.name.as_str()), Some("doomed"));
        assert_eq!(
            confirm.confirm(),
            ConfirmOutcome::Confirmed(PendingDelete {
                name: "doomed".into(),
                file_name: "doomed.mcvx".into(),
            })
        );
        // Confirmation is single-use: a second click must not re-delete.
        assert!(!confirm.is_pending());
        assert_eq!(confirm.confirm(), ConfirmOutcome::NothingPending);
    }

    #[test]
    fn delete_cancel_clears_and_replacing_a_request_never_confirms_the_old_row() {
        let first = CatalogRow::loadable("first".into(), "first.mcvx".into(), Some(5));
        let second = CatalogRow::loadable("second".into(), "second.mcvx".into(), Some(6));
        let mut confirm = DeleteConfirmation::new();

        confirm.request(&first);
        confirm.cancel();
        assert!(!confirm.is_pending());
        assert_eq!(confirm.confirm(), ConfirmOutcome::NothingPending);

        confirm.request(&first);
        confirm.request(&second); // new request replaces the stale one
        assert_eq!(
            confirm.confirm(),
            ConfirmOutcome::Confirmed(PendingDelete {
                name: "second".into(),
                file_name: "second.mcvx".into(),
            })
        );
    }
}
