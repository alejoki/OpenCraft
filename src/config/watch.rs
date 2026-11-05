//! Dev-only polling watcher for the curated config tree.
//!
//! MEM591/F10 rule: no `notify` and no platform watcher. The visible behavior
//! is a deterministic state machine — [`WatchState`] — driven by an injected
//! [`Instant`] and injected per-file snapshots, so tests never sleep and the
//! settle policy is decoupled from the operating system's event timing.
//! [`fingerprint_path`] is the thin filesystem helper that feeds it.
//!
//! Content fingerprints, not `(mtime, len)`: same-length edits such as
//! `4.6` → `5.6` are the common creator case and mtime granularity varies by
//! filesystem. Each file is tiny (the loader rejects anything above
//! `MAX_CONFIG_FILE_BYTES`), so hashing its bytes at ~10 Hz is cheap.
//!
//! [`CREATOR_TOOLS`] is the single release gate for the whole creator surface:
//! release builds still load `config/` at startup, but they never poll,
//! overlay, or trace.

use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use super::{
    ApplyMode, ConfigFile, ConfigRuntime, ConfigSeverity, ConfigValue, FileReload, ValueOrigin,
    REGISTRY,
};

/// Whether the dev-only creator surface (watcher, F6 overlay, trace) is
/// compiled in. Config loading itself is never gated.
pub const CREATOR_TOOLS: bool = cfg!(debug_assertions);

/// How often the app may poll the six config files.
pub const CONFIG_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long a file's latest snapshot must stay unchanged before it is treated
/// as a completed write. Editors save with temp+rename, truncation, or bursts
/// of short writes; every new snapshot restarts this window so only the final
/// bytes settle.
pub const CONFIG_SETTLE_INTERVAL: Duration = Duration::from_millis(250);

/// The compile-time file count is shared with the loader. If a registry file
/// is added or removed, `FILE_COUNT` and `ConfigFile::ALL` disagree and this
/// assertion fails the build instead of silently leaving a watch slot unused.
const _: () = assert!(super::FILE_COUNT == ConfigFile::ALL.len());

/// One file's content fingerprint.
///
/// `None` at the snapshot level means missing or unreadable; that is a legal
/// state, not an error, because deletion is a valid creator edit (it resets
/// the file to registry defaults).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileSnapshot(pub u64);

/// FNV-1a 64 over the file bytes.
///
/// Intentionally separate from the registry digest sink in the parent module:
/// the loader digest hashes effective values (formatting- and comment-blind)
/// and is persisted in saves, while this fingerprint hashes raw bytes and only
/// decides whether a poll should be re-parsed. Never uses `DefaultHasher`: the
/// result must be reproducible across processes, platforms, and Rust versions.
fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Fingerprint `path` without assuming it exists.
///
/// Metadata is consulted first. Any metadata error (missing file, permissions,
/// I/O) yields `None`, which the watcher treats as a deleted/unreadable
/// snapshot. Files above `MAX_CONFIG_FILE_BYTES` are fingerprinted from
/// `(len, modified)` instead of being read: a hostile 1 GiB file must never be
/// loaded ten times per second. Read errors also yield `None`; live reload then
/// reports a whole-file Error (or the missing-file warning) as data.
pub fn fingerprint_path(path: &Path) -> Option<FileSnapshot> {
    let metadata = std::fs::metadata(path).ok()?;

    if metadata.len() > super::MAX_CONFIG_FILE_BYTES {
        // mtime might be unavailable on exotic filesystems; length alone still
        // makes progress and keeps the output deterministic.
        let modified_nanos = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |elapsed| elapsed.as_nanos() as u64);
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&metadata.len().to_le_bytes());
        bytes[8..].copy_from_slice(&modified_nanos.to_le_bytes());
        return Some(FileSnapshot(fnv1a64(&bytes)));
    }

    let contents = std::fs::read(path).ok()?;
    Some(FileSnapshot(fnv1a64(&contents)))
}

/// Settle state for one file.
///
/// The doubled `Option` distinguishes "never observed" (`None`) from a real
/// baseline snapshot of `Some(None)` (observed as missing).
#[derive(Clone, Copy, Debug)]
struct FileWatch {
    last_observed: Option<Option<FileSnapshot>>,
    last_settled: Option<Option<FileSnapshot>>,
    pending: Option<(Option<FileSnapshot>, Instant)>,
}

impl FileWatch {
    const EMPTY: Self = Self {
        last_observed: None,
        last_settled: None,
        pending: None,
    };
}

/// Pure per-file settle machine plus poll scheduling.
///
/// The app calls [`Self::observe`] for every curated file on each poll and
/// reloads exactly the files for which it returns `true`.
pub struct WatchState {
    files: [FileWatch; super::FILE_COUNT],
    poll: Duration,
    settle: Duration,
    next_poll_at: Instant,
}

impl WatchState {
    /// Production timings: [`CONFIG_POLL_INTERVAL`] and
    /// [`CONFIG_SETTLE_INTERVAL`], first poll due immediately.
    pub fn new(now: Instant) -> Self {
        Self::with_timings(now, CONFIG_POLL_INTERVAL, CONFIG_SETTLE_INTERVAL)
    }

    /// Explicit timings for tests; production callers use [`Self::new`].
    pub fn with_timings(now: Instant, poll: Duration, settle: Duration) -> Self {
        Self {
            files: std::array::from_fn(|_| FileWatch::EMPTY),
            poll,
            settle,
            next_poll_at: now,
        }
    }

    /// Observe one file's latest snapshot.
    ///
    /// Returns `true` exactly once when the file's current snapshot has been
    /// stable for the settle interval and differs from the last settled
    /// snapshot. The first observation only records the baseline: startup has
    /// already loaded the tree, so it emits nothing.
    ///
    /// A snapshot that differs from the previous observation replaces the
    /// pending write and restarts the settle timer, which collapses rapid
    /// bursts to their final bytes. A pending snapshot that reverts to the
    /// currently settled bytes emits nothing. `None` is a legal snapshot
    /// (deleted or unreadable) and obeys the same rules.
    pub fn observe(
        &mut self,
        file: ConfigFile,
        snapshot: Option<FileSnapshot>,
        now: Instant,
    ) -> bool {
        let settle = self.settle;
        let watch = &mut self.files[super::file_index(file)];

        let Some(observed) = watch.last_observed else {
            // Startup already loaded this tree: the first poll is the baseline.
            watch.last_observed = Some(snapshot);
            watch.last_settled = Some(snapshot);
            return false;
        };

        if observed != snapshot {
            watch.last_observed = Some(snapshot);
            watch.pending = Some((snapshot, now));
            return false;
        }

        let Some((pending_snapshot, since)) = watch.pending else {
            return false;
        };
        if pending_snapshot != snapshot {
            // The newest bytes went back to the previously observed snapshot;
            // a pending write of some other snapshot can never settle now.
            return false;
        }
        if now.saturating_duration_since(since) < settle {
            return false;
        }

        watch.pending = None;
        if watch.last_settled == Some(snapshot) {
            // Reverted to the content that is already live: no reload needed.
            return false;
        }
        watch.last_settled = Some(snapshot);
        true
    }

    /// Files with an unsettled write, in [`ConfigFile::ALL`] order.
    pub fn pending_files(&self) -> Vec<ConfigFile> {
        ConfigFile::ALL
            .into_iter()
            .filter(|file| self.files[super::file_index(*file)].pending.is_some())
            .collect()
    }

    /// Whether the next scheduled poll is due.
    pub fn is_due(&self, now: Instant) -> bool {
        now >= self.next_poll_at
    }

    /// Schedule the next poll one poll interval after `now`.
    pub fn mark_polled(&mut self, now: Instant) {
        self.next_poll_at = now + self.poll;
    }

    /// When the next poll becomes due.
    pub fn next_poll_at(&self) -> Instant {
        self.next_poll_at
    }
}

/// Last completed reload event, shown on the overlay `last:` line.
///
/// This is presentation state only: the long rejection reason lives in
/// [`CreatorRuntime::last_error`] so the overlay can keep `last:` compact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReloadStatus {
    Idle,
    Applied {
        file: ConfigFile,
        changed: usize,
    },
    Rejected {
        file: ConfigFile,
        field: Option<String>,
    },
}

impl ReloadStatus {
    /// Stable one-line spelling for the overlay and diagnostics.
    pub fn display(&self) -> String {
        match self {
            ReloadStatus::Idle => "idle".to_string(),
            ReloadStatus::Applied { file, changed } => {
                format!("applied {} changed={changed}", file.rel_path())
            }
            ReloadStatus::Rejected { file, field } => match field {
                Some(key) => format!("rejected {} field={key}", file.rel_path()),
                None => format!("rejected {}", file.rel_path()),
            },
        }
    }
}

/// Dev-only creator state: watcher plus the overlay/restart facts the app
/// reads each frame. The struct owns no GPU, logging, or wall-clock state,
/// so every field is testable headlessly.
pub struct CreatorRuntime {
    pub watch: WatchState,
    pub overlay_visible: bool,
    pub status: ReloadStatus,
    pub last_error: Option<String>,
    pub restart_required: Option<String>,
}

impl CreatorRuntime {
    /// F6 overlay hidden, no reload observed yet, no restart pending.
    pub fn new(now: Instant) -> Self {
        Self {
            watch: WatchState::new(now),
            overlay_visible: false,
            status: ReloadStatus::Idle,
            last_error: None,
            restart_required: None,
        }
    }

    /// F6 toggles the read-only overlay.
    pub fn toggle_overlay(&mut self) {
        self.overlay_visible = !self.overlay_visible;
    }

    /// Fold one per-file reload outcome into the status surfaces.
    ///
    /// An apply clears the previous error; a rejection records the first
    /// Error's field (when keyed) and a single-line `config/<file>: ...`
    /// message for the transient game notice.
    pub fn record(&mut self, report: &FileReload) {
        if report.applied {
            self.status = ReloadStatus::Applied {
                file: report.file,
                changed: report.changed,
            };
            self.last_error = None;
            return;
        }

        let first = report.first_error();
        self.status = ReloadStatus::Rejected {
            file: report.file,
            field: first.and_then(|diagnostic| diagnostic.key.clone()),
        };
        self.last_error = Some(match first {
            Some(diagnostic) => match &diagnostic.key {
                Some(key) => {
                    format!("{}: {key}: {}", report.file.rel_path(), diagnostic.reason)
                }
                None => format!("{}: {}", report.file.rel_path(), diagnostic.reason),
            },
            // `reload_file` always produces an Error on rejection; keep a
            // truthful fallback rather than panicking on future callers.
            None => format!("{}: rejected", report.file.rel_path()),
        });
    }

    /// Hold a durable restart requirement (S03/S08); S02 only displays it.
    pub fn set_restart_required(&mut self, reason: impl Into<String>) {
        self.restart_required = Some(reason.into());
    }

    /// Clear a satisfied restart requirement.
    pub fn clear_restart_required(&mut self) {
        self.restart_required = None;
    }

    /// Files with an unsettled write, in [`ConfigFile::ALL`] order.
    pub fn pending_files(&self) -> Vec<ConfigFile> {
        self.watch.pending_files()
    }
}

/// `[live]`, `[restart]`, or `[live+restart]` for the rows in `applies`.
///
/// Derived from the registry, never hardcoded: adding the first
/// restart-only row updates both the legend and its file header.
fn apply_summary(applies: impl Iterator<Item = ApplyMode>) -> &'static str {
    let mut live = false;
    let mut restart = false;
    for apply in applies {
        match apply {
            ApplyMode::Live => live = true,
            ApplyMode::Restart => restart = true,
        }
    }
    match (live, restart) {
        (true, true) => "[live+restart]",
        (true, false) => "[live]",
        (false, true) => "[restart]",
        (false, false) => "[none]",
    }
}

/// Word-boundary wrap with hard splitting for single tokens wider than the
/// line. `indent` is prepended to every emitted line, so the returned lines
/// always satisfy `chars().count() <= max_chars.max(16)`.
fn wrap_text(text: &str, max_chars: usize, indent: &str) -> Vec<String> {
    // A floor keeps progress guaranteed even if a caller passes 0.
    let width = max_chars.max(16);
    let available = width.saturating_sub(indent.chars().count()).max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;

    let flush = |lines: &mut Vec<String>, current: &mut String, current_len: &mut usize| {
        if !current.is_empty() {
            lines.push(format!("{indent}{current}"));
            current.clear();
            *current_len = 0;
        }
    };

    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if word_len > available {
            flush(&mut lines, &mut current, &mut current_len);

            let mut chunk = String::new();
            for character in word.chars() {
                chunk.push(character);
                if chunk.chars().count() == available {
                    lines.push(format!("{indent}{chunk}"));
                    chunk.clear();
                }
            }
            current = chunk;
            current_len = current.chars().count();
            continue;
        }

        if current.is_empty() {
            current.push_str(word);
            current_len = word_len;
        } else if current_len + 1 + word_len <= available {
            current.push(' ');
            current.push_str(word);
            current_len += 1 + word_len;
        } else {
            flush(&mut lines, &mut current, &mut current_len);
            current.push_str(word);
            current_len = word_len;
        }
    }

    flush(&mut lines, &mut current, &mut current_len);
    lines
}

/// Render the complete read-only F6 overlay as explicit wrapped lines.
///
/// Everything is derived from registry metadata and the runtime's
/// provenance/diagnostics: there are no hardcoded keys, values, or apply
/// modes. The caller sizes the panel from `lines.len()`.
pub fn overlay_lines(
    runtime: &ConfigRuntime,
    creator: &CreatorRuntime,
    max_chars: usize,
) -> Vec<String> {
    let mut lines = Vec::new();

    let errors = runtime
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.severity == ConfigSeverity::Error)
        .count();
    let warnings = runtime
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.severity == ConfigSeverity::Warning)
        .count();
    let pending = creator.pending_files();
    let pending_text = if pending.is_empty() {
        "0".to_string()
    } else {
        pending
            .iter()
            .map(|file| file.name())
            .collect::<Vec<_>>()
            .join(",")
    };

    let header = format!(
        "CREATOR (dev) digest={:#018x} schema={} files={} pending={} warnings={} errors={}",
        runtime.digest(),
        runtime.schema(),
        ConfigFile::ALL.len(),
        pending_text,
        warnings,
        errors,
    );
    lines.extend(wrap_text(&header, max_chars, ""));

    let legend = format!(
        "(* default; unmarked = from file; {} = applies without restart)",
        apply_summary(REGISTRY.iter().map(|row| row.apply)),
    );
    lines.extend(wrap_text(&legend, max_chars, ""));

    lines.extend(wrap_text(
        &format!("last: {}", creator.status.display()),
        max_chars,
        "",
    ));

    if let Some(reason) = &creator.restart_required {
        lines.extend(wrap_text(
            &format!("restart required: {reason}"),
            max_chars,
            "",
        ));
    }

    for file in ConfigFile::ALL {
        let summary = apply_summary(
            REGISTRY
                .iter()
                .filter(|row| row.file == file)
                .map(|row| row.apply),
        );
        lines.extend(wrap_text(
            &format!("{} {summary}", file.rel_path()),
            max_chars,
            "",
        ));

        let tokens: Vec<String> = REGISTRY
            .iter()
            .filter(|row| row.file == file)
            .map(|row| {
                let value: ConfigValue = runtime.value(row.key);
                let mut token = format!("{}={}", row.key, value.display());
                if runtime.origin(row.key) == ValueOrigin::Default {
                    token.push('*');
                }
                token
            })
            .collect();
        lines.extend(wrap_text(&tokens.join(" "), max_chars, "  "));

        let file_errors: Vec<_> = runtime
            .diagnostics()
            .iter()
            .filter(|diagnostic| {
                diagnostic.file == file && diagnostic.severity == ConfigSeverity::Error
            })
            .collect();
        if let Some(first) = file_errors.first() {
            let error = match &first.key {
                Some(key) => format!("error: {key}: {}", first.reason),
                None => format!("error: {}", first.reason),
            };
            lines.extend(wrap_text(&error, max_chars, "  "));

            let more = file_errors.len() - 1;
            if more > 0 {
                lines.extend(wrap_text(
                    &format!("(+{more} more errors)"),
                    max_chars,
                    "  ",
                ));
            }
        }
    }

    lines
}

/// Collapse every run of whitespace so a trace line is always one line.
fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Stable `--creator-trace` output for one reloaded file.
///
/// Exactly one line, no timestamp and no absolute path, so shell smoke
/// tests can diff it across runs. The digest is the current runtime digest
/// and uses the same `{:#018x}` spelling as `--check-config`.
pub fn reload_trace_lines(report: &FileReload, digest: u64) -> Vec<String> {
    let file = report.file.rel_path();
    let line = if report.applied {
        format!(
            "creator: applied file={file} changed={} digest={digest:#018x}",
            report.changed
        )
    } else {
        match report.first_error() {
            Some(diagnostic) => match &diagnostic.key {
                Some(key) => format!(
                    "creator: rejected file={file} field={key} reason={}",
                    single_line(&diagnostic.reason)
                ),
                None => format!(
                    "creator: rejected file={file} reason={}",
                    single_line(&diagnostic.reason)
                ),
            },
            None => format!("creator: rejected file={file} reason=unknown"),
        }
    };
    vec![line]
}

#[cfg(test)]
mod tests {
    use super::super::ConfigDiagnostic;
    use super::*;
    use std::path::PathBuf;

    fn at(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    #[test]
    fn first_observation_is_a_silent_baseline() {
        let base = Instant::now();
        let mut watch = WatchState::new(base);

        // A first observation emits nothing even though it is not "nothing".
        assert!(!watch.observe(ConfigFile::Player, Some(FileSnapshot(1)), at(base, 0)));
        assert!(!watch.observe(ConfigFile::Player, Some(FileSnapshot(1)), at(base, 400)));
        assert!(watch.pending_files().is_empty());

        // A file that is already missing at startup also baselines silently.
        assert!(!watch.observe(ConfigFile::Survival, None, at(base, 0)));
        assert!(!watch.observe(ConfigFile::Survival, None, at(base, 400)));
        assert!(watch.pending_files().is_empty());
    }

    #[test]
    fn unchanged_snapshot_emits_nothing() {
        let base = Instant::now();
        let mut watch = WatchState::new(base);
        let snapshot = Some(FileSnapshot(7));

        assert!(!watch.observe(ConfigFile::Combat, snapshot, at(base, 0)));
        assert!(!watch.observe(ConfigFile::Combat, snapshot, at(base, 100)));
        assert!(!watch.observe(ConfigFile::Combat, snapshot, at(base, 10_000)));
        assert!(watch.pending_files().is_empty());
    }

    #[test]
    fn one_change_settles_exactly_once() {
        let base = Instant::now();
        let mut watch = WatchState::new(base);

        assert!(!watch.observe(ConfigFile::Player, Some(FileSnapshot(1)), at(base, 0)));
        assert!(!watch.observe(ConfigFile::Player, Some(FileSnapshot(2)), at(base, 0)));
        assert!(!watch.observe(ConfigFile::Player, Some(FileSnapshot(2)), at(base, 249)));
        assert_eq!(watch.pending_files(), vec![ConfigFile::Player]);

        assert!(watch.observe(ConfigFile::Player, Some(FileSnapshot(2)), at(base, 250)));
        assert!(watch.pending_files().is_empty());
        assert!(!watch.observe(ConfigFile::Player, Some(FileSnapshot(2)), at(base, 300)));
        assert!(!watch.observe(ConfigFile::Player, Some(FileSnapshot(2)), at(base, 10_000)));
    }

    #[test]
    fn rapid_burst_only_settles_the_final_bytes() {
        let base = Instant::now();
        let mut watch = WatchState::new(base);

        assert!(!watch.observe(ConfigFile::World, Some(FileSnapshot(1)), at(base, 0)));
        assert!(!watch.observe(ConfigFile::World, Some(FileSnapshot(2)), at(base, 10)));
        assert!(!watch.observe(ConfigFile::World, Some(FileSnapshot(3)), at(base, 100)));
        assert!(!watch.observe(ConfigFile::World, Some(FileSnapshot(4)), at(base, 200)));

        // The timer restarted at 200 ms, so the earlier timestamps cannot fire.
        assert!(!watch.observe(ConfigFile::World, Some(FileSnapshot(4)), at(base, 449)));
        assert!(watch.observe(ConfigFile::World, Some(FileSnapshot(4)), at(base, 450)));
        assert!(watch.pending_files().is_empty());
        assert!(!watch.observe(ConfigFile::World, Some(FileSnapshot(4)), at(base, 500)));
    }

    #[test]
    fn atomic_rename_sequence_settles_once() {
        let base = Instant::now();
        let mut watch = WatchState::new(base);

        assert!(!watch.observe(ConfigFile::Render, Some(FileSnapshot(11)), at(base, 0)));
        // Temp+rename can briefly expose "missing" before the new name lands.
        assert!(!watch.observe(ConfigFile::Render, None, at(base, 10)));
        assert!(!watch.observe(ConfigFile::Render, Some(FileSnapshot(22)), at(base, 20)));
        assert!(!watch.observe(ConfigFile::Render, Some(FileSnapshot(22)), at(base, 269)));
        assert!(watch.observe(ConfigFile::Render, Some(FileSnapshot(22)), at(base, 270)));
        assert!(!watch.observe(ConfigFile::Render, Some(FileSnapshot(22)), at(base, 400)));
    }

    #[test]
    fn partial_write_settles_and_the_completed_write_settles_again() {
        let base = Instant::now();
        let mut watch = WatchState::new(base);

        assert!(!watch.observe(ConfigFile::Mining, Some(FileSnapshot(1)), at(base, 0)));
        // Truncated bytes: the watcher cannot know; live reload rejects them.
        assert!(!watch.observe(ConfigFile::Mining, Some(FileSnapshot(2)), at(base, 10)));
        assert!(watch.observe(ConfigFile::Mining, Some(FileSnapshot(2)), at(base, 260)));
        // Completed write is a new snapshot and settles on its own timer.
        assert!(!watch.observe(ConfigFile::Mining, Some(FileSnapshot(3)), at(base, 300)));
        assert!(!watch.observe(ConfigFile::Mining, Some(FileSnapshot(3)), at(base, 549)));
        assert!(watch.observe(ConfigFile::Mining, Some(FileSnapshot(3)), at(base, 550)));
    }

    #[test]
    fn revert_to_last_settled_content_emits_nothing() {
        let base = Instant::now();
        let mut watch = WatchState::new(base);

        assert!(!watch.observe(ConfigFile::Player, Some(FileSnapshot(1)), at(base, 0)));
        assert!(!watch.observe(ConfigFile::Player, Some(FileSnapshot(2)), at(base, 10)));
        // Editor cancels the write and restores the live bytes before settling.
        assert!(!watch.observe(ConfigFile::Player, Some(FileSnapshot(1)), at(base, 20)));
        assert!(!watch.observe(ConfigFile::Player, Some(FileSnapshot(1)), at(base, 500)));
        assert!(watch.pending_files().is_empty());
    }

    #[test]
    fn deletion_settles_and_emits_once() {
        let base = Instant::now();
        let mut watch = WatchState::new(base);

        assert!(!watch.observe(ConfigFile::Survival, Some(FileSnapshot(1)), at(base, 0)));
        assert!(!watch.observe(ConfigFile::Survival, None, at(base, 10)));
        assert!(!watch.observe(ConfigFile::Survival, None, at(base, 259)));
        assert!(watch.observe(ConfigFile::Survival, None, at(base, 260)));
        assert!(!watch.observe(ConfigFile::Survival, None, at(base, 300)));
        assert!(!watch.observe(ConfigFile::Survival, None, at(base, 1_000)));
    }

    #[test]
    fn recreated_file_settles_and_emits_once() {
        let base = Instant::now();
        let mut watch = WatchState::new(base);

        assert!(!watch.observe(ConfigFile::World, Some(FileSnapshot(1)), at(base, 0)));
        assert!(!watch.observe(ConfigFile::World, None, at(base, 10)));
        assert!(watch.observe(ConfigFile::World, None, at(base, 260)));

        assert!(!watch.observe(ConfigFile::World, Some(FileSnapshot(2)), at(base, 300)));
        assert!(!watch.observe(ConfigFile::World, Some(FileSnapshot(2)), at(base, 549)));
        assert!(watch.observe(ConfigFile::World, Some(FileSnapshot(2)), at(base, 550)));
        assert!(watch.pending_files().is_empty());
    }

    #[test]
    fn pending_files_follow_config_file_order() {
        let base = Instant::now();
        let mut watch = WatchState::new(base);

        for file in ConfigFile::ALL {
            assert!(!watch.observe(file, Some(FileSnapshot(1)), at(base, 0)));
        }
        assert!(watch.pending_files().is_empty());

        assert!(!watch.observe(ConfigFile::Render, Some(FileSnapshot(2)), at(base, 10)));
        assert!(!watch.observe(ConfigFile::Player, Some(FileSnapshot(3)), at(base, 20)));
        assert_eq!(
            watch.pending_files(),
            vec![ConfigFile::Player, ConfigFile::Render]
        );

        assert!(watch.observe(ConfigFile::Player, Some(FileSnapshot(3)), at(base, 270)));
        assert_eq!(watch.pending_files(), vec![ConfigFile::Render]);
    }

    #[test]
    fn poll_schedule_uses_configured_interval() {
        let base = Instant::now();
        let mut watch =
            WatchState::with_timings(base, Duration::from_millis(100), Duration::from_millis(250));

        assert_eq!(watch.next_poll_at(), base);
        assert!(watch.is_due(base));
        assert!(!watch.is_due(base - Duration::from_millis(1)));

        watch.mark_polled(base);
        assert_eq!(watch.next_poll_at(), at(base, 100));
        assert!(!watch.is_due(at(base, 99)));
        assert!(watch.is_due(at(base, 100)));

        // mark_polled is anchored to the supplied now, not to the old deadline.
        watch.mark_polled(at(base, 500));
        assert_eq!(watch.next_poll_at(), at(base, 600));
    }

    #[test]
    fn production_constructor_uses_published_constants() {
        let base = Instant::now();
        let mut watch = WatchState::new(base);

        assert_eq!(watch.next_poll_at(), base);
        watch.mark_polled(base);
        assert_eq!(watch.next_poll_at(), base + CONFIG_POLL_INTERVAL);
    }

    #[test]
    fn custom_timings_take_precedence_over_the_constants() {
        let base = Instant::now();
        let mut watch =
            WatchState::with_timings(base, Duration::from_secs(1), Duration::from_secs(2));

        assert!(!watch.observe(ConfigFile::Player, Some(FileSnapshot(1)), at(base, 0)));
        assert!(!watch.observe(ConfigFile::Player, Some(FileSnapshot(2)), at(base, 0)));
        // Longer than CONFIG_SETTLE_INTERVAL but shorter than this instance's.
        assert!(!watch.observe(ConfigFile::Player, Some(FileSnapshot(2)), at(base, 250)));
        assert!(!watch.observe(ConfigFile::Player, Some(FileSnapshot(2)), at(base, 1_999)));
        assert!(watch.observe(ConfigFile::Player, Some(FileSnapshot(2)), at(base, 2_000)));

        watch.mark_polled(base);
        assert_eq!(watch.next_poll_at(), at(base, 1_000));
    }

    fn write_bytes(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn fingerprint_of_missing_path_is_none() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(fingerprint_path(&temp.path().join("absent.toml")), None);
    }

    #[test]
    fn fingerprint_of_unreadable_entry_is_none() {
        let temp = tempfile::tempdir().unwrap();
        // Directories pass metadata but fail `read`, modelling an unreadable file.
        assert_eq!(fingerprint_path(temp.path()), None);
    }

    #[test]
    fn identical_bytes_produce_identical_fingerprints() {
        let temp = tempfile::tempdir().unwrap();
        let left: PathBuf = temp.path().join("left.toml");
        let right: PathBuf = temp.path().join("right.toml");
        write_bytes(&left, b"[movement]\nwalk_speed = 4.6\n");
        write_bytes(&right, b"[movement]\nwalk_speed = 4.6\n");

        let left = fingerprint_path(&left);
        assert!(left.is_some());
        assert_eq!(left, fingerprint_path(&right));
    }

    #[test]
    fn same_length_different_bytes_produce_different_fingerprints() {
        let temp = tempfile::tempdir().unwrap();
        let left = temp.path().join("left.toml");
        let right = temp.path().join("right.toml");
        // Exactly the same length: mtime+len alone would miss this edit.
        write_bytes(&left, b"walk_speed = 4.6\n");
        write_bytes(&right, b"walk_speed = 5.6\n");

        assert_ne!(fingerprint_path(&left), fingerprint_path(&right));
    }

    #[test]
    fn one_byte_change_changes_the_fingerprint() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("player.toml");
        write_bytes(&path, b"[movement]\nwalk_speed = 4.6\n");
        let before = fingerprint_path(&path).unwrap();

        write_bytes(&path, b"[movement]\nwalk_speed = 5.6\n");
        let after = fingerprint_path(&path).unwrap();

        assert_ne!(before, after);
    }

    #[test]
    fn oversize_file_is_fingerprinted_from_metadata_only_and_is_stable() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("huge.toml");
        let size = (super::super::MAX_CONFIG_FILE_BYTES + 1) as usize;
        write_bytes(&path, &vec![b' '; size]);

        // It is `Some` even though the body is over the loader cap, and repeated
        // polls without a write are stable.
        let first = fingerprint_path(&path);
        assert!(first.is_some(), "oversize files still get a fingerprint");
        assert_eq!(first, fingerprint_path(&path));

        // Metadata fingerprint tracks length, so growth is still visible.
        write_bytes(&path, &vec![b' '; size + 1]);
        assert_ne!(first, fingerprint_path(&path));
    }

    // -----------------------------------------------------------------
    // CreatorRuntime, overlay, and trace
    // -----------------------------------------------------------------

    fn empty_config_dir() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        for file in ConfigFile::ALL {
            std::fs::write(temp.path().join(file.name()), b"").unwrap();
        }
        temp
    }

    fn applied_report(file: ConfigFile, changed: usize) -> FileReload {
        FileReload {
            file,
            applied: true,
            changed,
            diagnostics: Vec::new(),
        }
    }

    fn rejected_report(file: ConfigFile, key: Option<&str>, reason: &str) -> FileReload {
        FileReload {
            file,
            applied: false,
            changed: 0,
            diagnostics: vec![ConfigDiagnostic {
                file,
                key: key.map(str::to_string),
                severity: ConfigSeverity::Error,
                reason: reason.to_string(),
            }],
        }
    }

    #[test]
    fn overlay_header_carries_digest_schema_files_pending_and_counts() {
        let temp = empty_config_dir();
        std::fs::write(
            temp.path().join("player.toml"),
            "[movement]\nwalk_speed = 0.0\nmystery = 1\n",
        )
        .unwrap();
        let runtime = ConfigRuntime::load_dir(temp.path());
        assert_eq!(runtime.diagnostics().len(), 2, "one error plus one warning");

        let base = Instant::now();
        let mut creator = CreatorRuntime::new(base);
        // Baseline, then leave one file with an unsettled write.
        assert!(!creator
            .watch
            .observe(ConfigFile::Player, Some(FileSnapshot(1)), base));
        assert!(!creator
            .watch
            .observe(ConfigFile::Player, Some(FileSnapshot(2)), at(base, 10)));
        assert_eq!(creator.pending_files(), vec![ConfigFile::Player]);

        let lines = overlay_lines(&runtime, &creator, 140);
        let header = &lines[0];
        assert!(
            header.contains(&format!("digest={:#018x}", runtime.digest())),
            "{header}"
        );
        assert!(header.contains("schema=1"), "{header}");
        assert!(header.contains("files=6"), "{header}");
        assert!(header.contains("pending=player.toml"), "{header}");
        assert!(header.contains("warnings=1"), "{header}");
        assert!(header.contains("errors=1"), "{header}");
    }

    #[test]
    fn overlay_groups_every_registry_row_by_file_with_source_markers() {
        let temp = empty_config_dir();
        let walk_index = super::super::index_of("movement.walk_speed").unwrap();
        let walk_default = REGISTRY[walk_index].default;
        std::fs::write(
            temp.path().join("player.toml"),
            format!("[movement]\nwalk_speed = {}\n", walk_default.display()),
        )
        .unwrap();
        let runtime = ConfigRuntime::load_dir(temp.path());
        let creator = CreatorRuntime::new(Instant::now());
        let lines = overlay_lines(&runtime, &creator, 140);
        let joined = lines.join(" ");

        for row in REGISTRY {
            assert!(
                joined.contains(&format!("{}=", row.key)),
                "overlay is missing {}",
                row.key
            );
        }
        for file in ConfigFile::ALL {
            assert!(
                lines
                    .iter()
                    .any(|line| line.starts_with(&format!("{} ", file.rel_path()))),
                "overlay is missing the {} group header",
                file.name()
            );
        }
        assert!(lines.iter().any(|line| line.contains("[live]")));

        // A file value equal to the default has no `*`: provenance, not equality.
        assert_eq!(
            runtime.origin("movement.walk_speed"),
            ValueOrigin::File(ConfigFile::Player)
        );
        let walk_token = joined
            .split_whitespace()
            .find(|token| token.starts_with("movement.walk_speed="))
            .expect("walk_speed token");
        assert_eq!(
            walk_token,
            format!("movement.walk_speed={}", walk_default.display())
        );

        let sprint_index = super::super::index_of("movement.sprint_speed").unwrap();
        let sprint_default = REGISTRY[sprint_index].default;
        let sprint_token = joined
            .split_whitespace()
            .find(|token| token.starts_with("movement.sprint_speed="))
            .expect("sprint_speed token");
        assert_eq!(
            sprint_token,
            format!("movement.sprint_speed={}*", sprint_default.display())
        );
    }

    #[test]
    fn overlay_last_and_restart_lines_reflect_creator_state() {
        let runtime = ConfigRuntime::defaults();
        let mut creator = CreatorRuntime::new(Instant::now());

        let before = overlay_lines(&runtime, &creator, 140).join("\n");
        assert!(before.contains("last: idle"), "{before}");
        assert!(!before.contains("restart required:"));

        creator.record(&applied_report(ConfigFile::Player, 2));
        let applied = overlay_lines(&runtime, &creator, 140).join("\n");
        assert!(
            applied.contains("last: applied config/player.toml changed=2"),
            "{applied}"
        );

        creator.record(&rejected_report(
            ConfigFile::Player,
            Some("movement.walk_speed"),
            "value 0 is below minimum 0.5",
        ));
        let rejected = overlay_lines(&runtime, &creator, 140).join("\n");
        assert!(
            rejected.contains("last: rejected config/player.toml field=movement.walk_speed"),
            "{rejected}"
        );

        creator.set_restart_required("recipe changed");
        let restart = overlay_lines(&runtime, &creator, 140).join("\n");
        assert!(restart.contains("restart required: recipe changed"));

        creator.clear_restart_required();
        let cleared = overlay_lines(&runtime, &creator, 140).join("\n");
        assert!(!cleared.contains("restart required:"));
    }

    #[test]
    fn overlay_shows_first_error_and_more_count_while_keeping_last_good_values() {
        let temp = empty_config_dir();
        std::fs::write(
            temp.path().join("player.toml"),
            "[movement]\nwalk_speed = 6.0\n",
        )
        .unwrap();
        let mut runtime = ConfigRuntime::load_dir(temp.path());
        assert_eq!(runtime.f32("movement.walk_speed"), 6.0);

        std::fs::write(
            temp.path().join("player.toml"),
            "[movement]\nwalk_speed = 0.0\nsprint_speed = 0.0\n",
        )
        .unwrap();
        let report = runtime.reload_file(temp.path(), ConfigFile::Player);
        assert!(!report.applied);
        assert_eq!(report.error_count(), 2);

        let mut creator = CreatorRuntime::new(Instant::now());
        creator.record(&report);
        let lines = overlay_lines(&runtime, &creator, 140);
        let joined = lines.join("\n");

        assert!(joined.contains("movement.walk_speed=6.0"), "{joined}");
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.trim_start().starts_with("error:"))
                .count(),
            1,
            "exactly the first error is rendered"
        );
        assert!(joined.contains("error: movement.walk_speed:"), "{joined}");
        assert!(joined.contains("(+1 more errors)"), "{joined}");
    }

    #[test]
    fn overlay_respects_line_budget_at_supported_widths() {
        let temp = empty_config_dir();
        let runtime = ConfigRuntime::load_dir(temp.path());
        let creator = CreatorRuntime::new(Instant::now());

        let wide = overlay_lines(&runtime, &creator, 220);
        let narrow = overlay_lines(&runtime, &creator, 140);
        let tiny = overlay_lines(&runtime, &creator, 16);

        assert!(
            narrow.iter().all(|line| line.chars().count() <= 140),
            "a 140-column line is too wide"
        );
        assert!(
            narrow.len() <= 40,
            "narrow overlay used {} lines",
            narrow.len()
        );
        assert!(wide.iter().all(|line| line.chars().count() <= 220));
        assert!(wide.len() <= narrow.len());
        assert!(tiny.iter().all(|line| line.chars().count() <= 16));
        assert!(
            tiny.iter()
                .any(|line| line.chars().count() == 16 && !line.trim().contains(' ')),
            "expected at least one hard-split long token"
        );
    }

    #[test]
    fn trace_lines_are_exact_and_stable() {
        let applied = applied_report(ConfigFile::Player, 2);
        let expected_applied =
            "creator: applied file=config/player.toml changed=2 digest=0x00000000000000ab";
        assert_eq!(
            reload_trace_lines(&applied, 0x0000_0000_0000_00ab),
            vec![expected_applied.to_string()]
        );
        // Identical inputs produce byte-identical output: no timestamps.
        assert_eq!(
            reload_trace_lines(&applied, 0x0000_0000_0000_00ab),
            reload_trace_lines(&applied, 0x0000_0000_0000_00ab)
        );

        let keyed = rejected_report(
            ConfigFile::Player,
            Some("movement.walk_speed"),
            "value 0 is below minimum 0.5",
        );
        assert_eq!(
            reload_trace_lines(&keyed, 0),
            vec![
                "creator: rejected file=config/player.toml field=movement.walk_speed \
                 reason=value 0 is below minimum 0.5"
                    .to_string()
            ]
        );

        let file_level = rejected_report(
            ConfigFile::Player,
            None,
            "invalid TOML: expected `]` at line 1 column 12",
        );
        assert_eq!(
            reload_trace_lines(&file_level, 0),
            vec!["creator: rejected file=config/player.toml \
                 reason=invalid TOML: expected `]` at line 1 column 12"
                .to_string()]
        );
    }

    #[test]
    fn trace_reason_is_collapsed_to_one_line() {
        let report = rejected_report(
            ConfigFile::World,
            Some("spawning.spawn_max_radius_blocks"),
            "line one\nline\ttwo   three",
        );
        let lines = reload_trace_lines(&report, 0);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0],
            "creator: rejected file=config/world.toml field=spawning.spawn_max_radius_blocks \
             reason=line one line two three"
        );
        assert!(!lines[0].contains('\n') && !lines[0].contains('\t'));
    }

    #[test]
    fn creator_record_clears_errors_on_apply_and_prefixes_rejects() {
        let mut creator = CreatorRuntime::new(Instant::now());

        creator.record(&rejected_report(
            ConfigFile::Player,
            Some("movement.walk_speed"),
            "value 0 is below minimum 0.5",
        ));
        assert_eq!(
            creator.last_error.as_deref(),
            Some("config/player.toml: movement.walk_speed: value 0 is below minimum 0.5")
        );
        assert_eq!(
            creator.status,
            ReloadStatus::Rejected {
                file: ConfigFile::Player,
                field: Some("movement.walk_speed".to_string()),
            }
        );

        creator.record(&rejected_report(
            ConfigFile::World,
            None,
            "invalid TOML: expected `]`",
        ));
        assert_eq!(
            creator.last_error.as_deref(),
            Some("config/world.toml: invalid TOML: expected `]`")
        );
        assert_eq!(
            creator.status,
            ReloadStatus::Rejected {
                file: ConfigFile::World,
                field: None,
            }
        );

        creator.record(&applied_report(ConfigFile::Render, 1));
        assert_eq!(creator.last_error, None);
        assert_eq!(
            creator.status,
            ReloadStatus::Applied {
                file: ConfigFile::Render,
                changed: 1,
            }
        );
    }

    #[test]
    fn creator_tools_matches_the_build_profile() {
        assert_eq!(CREATOR_TOOLS, cfg!(debug_assertions));
    }
}
