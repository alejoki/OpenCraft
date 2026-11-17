//! Neutral per-field pack warning.
//!
//! The pack parser reports every fallback as data instead of logging or
//! failing, exactly like the game's settings loader. The crate cannot name the
//! game's `SettingsWarning`, so it owns this minimal shape and the game
//! converts where its own type is required.

/// One authored pack field that fell back, and why.
///
/// `field` is the authored file, manifest key or pack folder the failure was
/// attributed to; `reason` is the human-readable fallback reason. Both strings
/// are pinned by tests: the loader, validator and watcher print them verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackWarning {
    pub field: String,
    pub reason: String,
}
