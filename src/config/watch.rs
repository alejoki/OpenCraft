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

use super::ConfigFile;

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

#[cfg(test)]
mod tests {
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
}
