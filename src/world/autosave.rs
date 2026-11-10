//! Off-thread streaming-autosave worker.
//!
//! The frame thread projects the session into a [`WorldSaveData`] snapshot,
//! hands that snapshot plus the registry, path, and config metadata to this
//! worker, and returns immediately. Encoding and the atomic disk write happen
//! on the worker thread through the unchanged single v9 writer
//! ([`save_world_to_path_with_config`], D268), so the frame thread pays only
//! the snapshot cost.
//!
//! Exactly one save may be in flight at a time. Only the frame thread calls
//! [`AutosaveWorker::dispatch`] or [`AutosaveWorker::reap_finished`], so the
//! presence of a join handle doubles as the in-flight flag; the worker never
//! clears it (that is the autosave equivalent of D268's single-writer rule).
//! A request that arrives while a write is in flight is recorded as `pending`
//! and must be dispatched by the frame thread once the worker is reaped.
//!
//! The module is complete ahead of its frame-loop wiring: the next slice task
//! owns the dispatch/reap call sites, so the non-test library build allows
//! the as-yet-unreferenced items.
#![cfg_attr(not(test), allow(dead_code))]

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use crate::config::SaveConfigMeta;
use crate::content::ContentRegistry;
use crate::world::persistence::{
    save_world_to_path_with_config, PersistenceError, WorldSaveData,
};

/// Frame-thread-owned handle to the single streaming-autosave worker.
pub(crate) struct AutosaveWorker {
    /// Join handle for the one in-flight write. `Some` is the in-flight flag.
    handle: Option<JoinHandle<Result<(), PersistenceError>>>,
    /// A save was requested while a write was already in flight.
    pending: bool,
    /// Shared with the worker so a synchronous save can exclude it.
    write_lock: Arc<Mutex<()>>,
}

impl AutosaveWorker {
    pub(crate) fn new() -> Self {
        Self {
            handle: None,
            pending: false,
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Whether a worker thread is currently owned (running or finished but
    /// not yet reaped). Only the frame thread clears this.
    pub(crate) fn in_flight(&self) -> bool {
        self.handle.is_some()
    }

    /// Whether the in-flight worker has finished and can be reaped.
    pub(crate) fn has_finished_worker(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
    }

    /// Spawn the worker to encode and atomically write `data`.
    ///
    /// Returns immediately on success. A spawn failure is reported as
    /// [`PersistenceError::Io`] and leaves the caller's dirty flag untouched.
    pub(crate) fn dispatch(
        &mut self,
        data: WorldSaveData,
        path: PathBuf,
        registry: Arc<ContentRegistry>,
        config: SaveConfigMeta,
    ) -> Result<(), PersistenceError> {
        debug_assert!(
            !self.in_flight(),
            "autosave dispatch while a worker is in flight"
        );
        let write_lock = Arc::clone(&self.write_lock);
        let handle = std::thread::Builder::new()
            .name("autosave".into())
            .spawn(move || {
                let _guard = write_lock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                save_world_to_path_with_config(&path, &data, registry.as_ref(), config)
            })
            .map_err(PersistenceError::Io)?;
        self.handle = Some(handle);
        Ok(())
    }

    /// Take and join the worker only when it has already finished.
    pub(crate) fn reap_finished(&mut self) -> Option<Result<(), PersistenceError>> {
        if !self.has_finished_worker() {
            return None;
        }
        let handle = self.handle.take()?;
        Some(join_mapped(handle))
    }

    /// Join the in-flight worker, blocking until it finishes.
    pub(crate) fn wait(&mut self) -> Option<Result<(), PersistenceError>> {
        let handle = self.handle.take()?;
        Some(join_mapped(handle))
    }

    pub(crate) fn request_pending(&mut self) {
        self.pending = true;
    }

    pub(crate) fn take_pending(&mut self) -> bool {
        std::mem::take(&mut self.pending)
    }

    pub(crate) fn clear_pending(&mut self) {
        self.pending = false;
    }

    /// Exclude the worker from writing while the caller performs a
    /// synchronous save (quit/F5). Poisoning is ignored: an earlier worker
    /// panic must not wedge every later save.
    // Wired into the quit/F5 synchronous-save path by the next slice task.
    pub(crate) fn acquire_write_lock(&self) -> MutexGuard<'_, ()> {
        self.write_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Deterministic test seam: a worker that panics after taking the write
    /// lock.
    #[cfg(test)]
    pub(crate) fn dispatch_panicking(&mut self) {
        let write_lock = Arc::clone(&self.write_lock);
        let handle = std::thread::Builder::new()
            .name("autosave-test-panic".into())
            .spawn(move || -> Result<(), PersistenceError> {
                let _guard = write_lock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                panic!("autosave test panic");
            })
            .expect("test worker spawns");
        self.handle = Some(handle);
    }

    /// Deterministic test seam: clone the same write lock the worker uses so
    /// a test can hold it without borrowing the worker itself.
    #[cfg(test)]
    pub(crate) fn lock_for_test(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.write_lock)
    }
}

impl Drop for AutosaveWorker {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Join a worker, converting a panic payload into an I/O error.
///
/// A panicking worker is a bug, not a data-loss path: the save did not
/// complete, so the caller keeps the world dirty. D006 forbids logging, so
/// this conversion is the only report.
fn join_mapped(
    handle: JoinHandle<Result<(), PersistenceError>>,
) -> Result<(), PersistenceError> {
    match handle.join() {
        Ok(result) => result,
        Err(_panic_payload) => Err(PersistenceError::Io(io::Error::other(
            "autosave worker panicked",
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::world::persistence::save_world_to_bytes_with_config;
    use crate::world::season::DEFAULT_SEASON_LENGTH_DAYS;
    use crate::world::session::WorldSession;

    fn sample_session(path: PathBuf) -> WorldSession {
        WorldSession::new_session(7_331, path, DEFAULT_SEASON_LENGTH_DAYS)
    }

    #[test]
    fn worker_writes_the_snapshot_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("world.mcvx");
        let session = sample_session(path.clone());
        let data = session.to_save_data();
        let registry = session.registry.clone();
        let config = session.save_config();

        let mut worker = AutosaveWorker::new();
        worker
            .dispatch(data.clone(), path.clone(), registry.clone(), config)
            .expect("dispatch succeeds");
        assert!(matches!(worker.wait(), Some(Ok(()))), "worker save succeeds");

        let written = std::fs::read(&path).expect("save file readable");
        let mut expected = Vec::new();
        save_world_to_bytes_with_config(&mut expected, &data, registry.as_ref(), config)
            .expect("expected bytes encode");
        assert_eq!(written, expected, "worker writes the unchanged v9 bytes");
    }

    #[test]
    fn worker_tracks_in_flight_and_pending() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("world.mcvx");
        let session = sample_session(path.clone());
        let data = session.to_save_data();
        let registry = session.registry.clone();
        let config = session.save_config();

        let mut worker = AutosaveWorker::new();
        assert!(!worker.in_flight());

        let lock = worker.lock_for_test();
        let guard = lock.lock().expect("test lock");
        worker
            .dispatch(data, path, registry, config)
            .expect("dispatch succeeds");
        assert!(worker.in_flight());
        assert!(!worker.has_finished_worker(), "worker blocks on the lock");

        worker.request_pending();
        assert!(worker.take_pending());
        assert!(!worker.take_pending(), "take_pending clears the request");
        worker.request_pending();
        worker.clear_pending();
        assert!(!worker.take_pending());

        drop(guard);
        assert!(matches!(worker.wait(), Some(Ok(()))), "worker save succeeds");
        assert!(!worker.in_flight());
    }

    #[test]
    fn worker_panic_becomes_io_error() {
        let mut worker = AutosaveWorker::new();
        worker.dispatch_panicking();
        let result = worker.wait();
        assert!(
            matches!(result, Some(Err(PersistenceError::Io(_)))),
            "panic payload maps to an I/O error, got {result:?}"
        );
        assert!(!worker.in_flight());
    }

    #[test]
    fn worker_wait_without_a_dispatch_is_none() {
        let mut worker = AutosaveWorker::new();
        drop(worker.acquire_write_lock());
        assert!(worker.wait().is_none());
        assert!(worker.reap_finished().is_none());
    }

    #[test]
    fn write_lock_serializes_worker_writes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("world.mcvx");
        let session = sample_session(path.clone());
        let data = session.to_save_data();
        let registry = session.registry.clone();
        let config = session.save_config();

        let mut worker = AutosaveWorker::new();
        let lock = worker.lock_for_test();
        let guard = lock.lock().expect("test lock");
        worker
            .dispatch(data, path, registry, config)
            .expect("dispatch succeeds");
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            !worker.has_finished_worker(),
            "the synchronous write lock holds the worker off disk"
        );
        assert!(worker.reap_finished().is_none());

        drop(guard);
        assert!(matches!(worker.wait(), Some(Ok(()))), "worker save succeeds");
    }
}
