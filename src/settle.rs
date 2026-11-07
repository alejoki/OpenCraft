//! Pure settle machine shared by the config and pack watchers.
//!
//! Both dev-only watchers poll a dynamic list of files and must apply a change
//! only after the file's latest bytes have been stable for one settle window.
//! The rules live here once so the config watcher and the pack watcher cannot
//! drift: the first observation baselines silently, every new snapshot
//! restarts the pending timer, a snapshot stable for the settle interval
//! emits `true` exactly once, a revert to the settled snapshot emits nothing,
//! and `None` is a legal snapshot (deleted or unreadable).
//!
//! The machine is clock-injected and filesystem-free. Callers own the
//! fingerprinting and the reaction to a settled entry.

use std::time::Duration;
use std::time::Instant;

/// Settle state for one tracked entry.
///
/// The doubled `Option` distinguishes "never observed" (`None`) from a real
/// baseline snapshot of `Some(None)` (observed as missing).
#[derive(Clone, Copy, Debug)]
struct Slot<T> {
    last_observed: Option<Option<T>>,
    last_settled: Option<Option<T>>,
    pending: Option<(Option<T>, Instant)>,
}

impl<T> Slot<T> {
    const EMPTY: Self = Self {
        last_observed: None,
        last_settled: None,
        pending: None,
    };
}

/// Generic per-entry settle machine plus poll scheduling.
///
/// `T` is the caller's content fingerprint. It must be cheap to copy because a
/// snapshot is retained twice per entry while a write is pending.
pub struct SettleTracker<T: Copy + PartialEq> {
    slots: Vec<Slot<T>>,
    poll: Duration,
    settle: Duration,
    next_poll_at: Instant,
}

impl<T: Copy + PartialEq> SettleTracker<T> {
    /// Create `count` unobserved entries with explicit timings and an
    /// immediate first poll.
    pub fn new(count: usize, poll: Duration, settle: Duration, now: Instant) -> Self {
        Self {
            slots: (0..count).map(|_| Slot::EMPTY).collect(),
            poll,
            settle,
            next_poll_at: now,
        }
    }

    /// Observe one entry's latest snapshot.
    ///
    /// Returns `true` exactly once when the entry's current snapshot has been
    /// stable for the settle interval and differs from the last settled
    /// snapshot. The first observation only records the baseline: startup has
    /// already loaded the tree, so it emits nothing.
    ///
    /// A snapshot that differs from the previous observation replaces the
    /// pending write and restarts the settle timer, which collapses rapid
    /// bursts to their final bytes. A pending snapshot that reverts to the
    /// currently settled bytes emits nothing. `None` is a legal snapshot
    /// (deleted or unreadable) and obeys the same rules.
    ///
    /// An out-of-range index debug-asserts and returns `false`, so a release
    /// build never panics on a stale entry list.
    pub fn observe(&mut self, index: usize, snapshot: Option<T>, now: Instant) -> bool {
        let Some(slot) = self.slots.get_mut(index) else {
            debug_assert!(false, "settle index {index} is out of range");
            return false;
        };

        let Some(observed) = slot.last_observed else {
            // Startup already loaded this entry: the first poll is the baseline.
            slot.last_observed = Some(snapshot);
            slot.last_settled = Some(snapshot);
            return false;
        };

        if observed != snapshot {
            slot.last_observed = Some(snapshot);
            slot.pending = Some((snapshot, now));
            return false;
        }

        let Some((pending_snapshot, since)) = slot.pending else {
            return false;
        };
        if pending_snapshot != snapshot {
            // The newest bytes went back to the previously observed snapshot;
            // a pending write of some other snapshot can never settle now.
            return false;
        }
        if now.saturating_duration_since(since) < self.settle {
            return false;
        }

        slot.pending = None;
        if slot.last_settled == Some(snapshot) {
            // Reverted to the content that is already live: no reload needed.
            return false;
        }
        slot.last_settled = Some(snapshot);
        true
    }

    /// Indices with an unsettled write, in ascending order.
    pub fn pending_indices(&self) -> Vec<usize> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.pending.is_some())
            .map(|(index, _)| index)
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

    fn at(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    fn tracker(count: usize, base: Instant) -> SettleTracker<u64> {
        SettleTracker::new(
            count,
            Duration::from_millis(100),
            Duration::from_millis(250),
            base,
        )
    }

    #[test]
    fn first_observation_is_a_silent_baseline() {
        let base = Instant::now();
        let mut tracker = tracker(2, base);

        assert!(!tracker.observe(0, Some(1), at(base, 0)));
        assert!(!tracker.observe(0, Some(1), at(base, 400)));
        assert!(tracker.pending_indices().is_empty());

        // None is a legal baseline: a file missing at startup is observed,
        // not treated as an error.
        assert!(!tracker.observe(1, None, at(base, 0)));
        assert!(!tracker.observe(1, None, at(base, 400)));
        assert!(tracker.pending_indices().is_empty());
    }

    #[test]
    fn one_change_settles_exactly_once() {
        let base = Instant::now();
        let mut tracker = tracker(1, base);

        assert!(!tracker.observe(0, Some(1), at(base, 0)));
        assert!(!tracker.observe(0, Some(2), at(base, 0)));
        assert!(!tracker.observe(0, Some(2), at(base, 249)));
        assert_eq!(tracker.pending_indices(), vec![0]);

        assert!(tracker.observe(0, Some(2), at(base, 250)));
        assert!(tracker.pending_indices().is_empty());
        assert!(!tracker.observe(0, Some(2), at(base, 300)));
        assert!(!tracker.observe(0, Some(2), at(base, 10_000)));
    }

    #[test]
    fn rapid_burst_only_settles_the_final_bytes() {
        let base = Instant::now();
        let mut tracker = tracker(1, base);

        assert!(!tracker.observe(0, Some(1), at(base, 0)));
        assert!(!tracker.observe(0, Some(2), at(base, 10)));
        assert!(!tracker.observe(0, Some(3), at(base, 100)));
        assert!(!tracker.observe(0, Some(4), at(base, 200)));

        // The timer restarted at 200 ms, so the earlier timestamps cannot fire.
        assert!(!tracker.observe(0, Some(4), at(base, 449)));
        assert!(tracker.observe(0, Some(4), at(base, 450)));
        assert!(tracker.pending_indices().is_empty());
        assert!(!tracker.observe(0, Some(4), at(base, 500)));
    }

    #[test]
    fn atomic_rename_sequence_applies_once_with_final_bytes() {
        let base = Instant::now();
        let mut tracker = tracker(1, base);

        assert!(!tracker.observe(0, Some(11), at(base, 0)));
        // Temp+rename can briefly expose "missing" before the new name lands.
        assert!(!tracker.observe(0, None, at(base, 10)));
        assert!(!tracker.observe(0, Some(22), at(base, 20)));
        assert!(!tracker.observe(0, Some(22), at(base, 269)));
        assert!(tracker.observe(0, Some(22), at(base, 270)));
        assert!(!tracker.observe(0, Some(22), at(base, 400)));
    }

    #[test]
    fn partial_write_settles_only_its_final_snapshot() {
        let base = Instant::now();
        let mut tracker = tracker(1, base);

        assert!(!tracker.observe(0, Some(1), at(base, 0)));
        // Truncated bytes A are replaced by completed bytes B before settling:
        // only B may be reported.
        assert!(!tracker.observe(0, Some(2), at(base, 10)));
        assert!(!tracker.observe(0, Some(3), at(base, 100)));
        assert!(!tracker.observe(0, Some(3), at(base, 349)));
        assert!(tracker.observe(0, Some(3), at(base, 350)));
    }

    #[test]
    fn revert_to_last_settled_snapshot_emits_nothing() {
        let base = Instant::now();
        let mut tracker = tracker(1, base);

        assert!(!tracker.observe(0, Some(1), at(base, 0)));
        assert!(!tracker.observe(0, Some(2), at(base, 10)));
        // Editor cancels the write and restores the live bytes before settling.
        assert!(!tracker.observe(0, Some(1), at(base, 20)));
        assert!(!tracker.observe(0, Some(1), at(base, 500)));
        assert!(tracker.pending_indices().is_empty());
    }

    #[test]
    fn deletion_settles_and_emits_once() {
        let base = Instant::now();
        let mut tracker = tracker(1, base);

        assert!(!tracker.observe(0, Some(1), at(base, 0)));
        assert!(!tracker.observe(0, None, at(base, 10)));
        assert!(!tracker.observe(0, None, at(base, 259)));
        assert!(tracker.observe(0, None, at(base, 260)));
        assert!(!tracker.observe(0, None, at(base, 300)));
        assert!(!tracker.observe(0, None, at(base, 1_000)));
    }

    #[test]
    fn recreated_entry_settles_and_emits_once() {
        let base = Instant::now();
        let mut tracker = tracker(1, base);

        assert!(!tracker.observe(0, Some(1), at(base, 0)));
        assert!(!tracker.observe(0, None, at(base, 10)));
        assert!(tracker.observe(0, None, at(base, 260)));

        assert!(!tracker.observe(0, Some(2), at(base, 300)));
        assert!(!tracker.observe(0, Some(2), at(base, 549)));
        assert!(tracker.observe(0, Some(2), at(base, 550)));
        assert!(tracker.pending_indices().is_empty());
    }

    #[test]
    fn pending_indices_follow_entry_order() {
        let base = Instant::now();
        let mut tracker = tracker(3, base);

        for index in 0..3 {
            assert!(!tracker.observe(index, Some(1), at(base, 0)));
        }
        assert!(tracker.pending_indices().is_empty());

        assert!(!tracker.observe(2, Some(3), at(base, 10)));
        assert!(!tracker.observe(0, Some(2), at(base, 20)));
        assert_eq!(tracker.pending_indices(), vec![0, 2]);

        assert!(tracker.observe(0, Some(2), at(base, 270)));
        assert_eq!(tracker.pending_indices(), vec![2]);
    }

    #[test]
    fn independent_entries_keep_independent_settle_timers() {
        let base = Instant::now();
        let mut tracker = tracker(2, base);

        assert!(!tracker.observe(0, Some(1), at(base, 0)));
        assert!(!tracker.observe(1, Some(1), at(base, 0)));
        assert!(!tracker.observe(0, Some(2), at(base, 0)));
        assert!(!tracker.observe(1, Some(2), at(base, 100)));

        assert!(tracker.observe(0, Some(2), at(base, 250)));
        assert!(!tracker.observe(1, Some(2), at(base, 250)));
        assert!(tracker.observe(1, Some(2), at(base, 350)));
    }

    #[test]
    fn poll_schedule_uses_configured_interval() {
        let base = Instant::now();
        let mut tracker = tracker(0, base);

        assert_eq!(tracker.next_poll_at(), base);
        assert!(tracker.is_due(base));
        assert!(!tracker.is_due(base - Duration::from_millis(1)));

        tracker.mark_polled(base);
        assert_eq!(tracker.next_poll_at(), at(base, 100));
        assert!(!tracker.is_due(at(base, 99)));
        assert!(tracker.is_due(at(base, 100)));

        // mark_polled is anchored to the supplied now, not to the old deadline.
        tracker.mark_polled(at(base, 500));
        assert_eq!(tracker.next_poll_at(), at(base, 600));
    }

    #[test]
    fn empty_tracker_is_supported() {
        let base = Instant::now();
        let mut tracker = tracker(0, base);

        assert!(tracker.pending_indices().is_empty());
        assert!(tracker.is_due(base));
        tracker.mark_polled(base);
        assert_eq!(tracker.next_poll_at(), at(base, 100));
    }
}
