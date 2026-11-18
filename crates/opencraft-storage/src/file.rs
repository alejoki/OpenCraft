//! Bounded file opening/reading and atomic whole-file writes.
//!
//! These are the game's proven save-file mechanics moved verbatim into the
//! neutral crate: every path is checked to be a regular file *before* it is
//! opened (so a FIFO or socket can never block the caller), every byte length
//! is checked against a caller-supplied limit bounded by
//! [`MAX_ARCHIVE_BYTES`], and writes land in a sibling `.mcvx.tmp` file that is
//! flushed, synced and renamed over the target, so a failed write never
//! replaces the previous save.
//!
//! The limit is `u64` and every length is checked before it is converted to
//! `usize`; the crate never trusts a length it read from disk.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::Path;

use crate::archive::MAX_ARCHIVE_BYTES;
use crate::StorageError;

/// Rejects `len > limit` with a typed [`StorageError::TooLarge`].
///
/// Used both for caller limits (checked against [`MAX_ARCHIVE_BYTES`]) and for
/// observed file lengths, so the reason is always carried in the error.
fn reject_over_limit(len: u64, limit: u64) -> Result<(), StorageError> {
    if len > limit {
        return Err(StorageError::TooLarge { len, limit });
    }
    Ok(())
}

/// Opens `path` for reading, rejecting anything that is not a regular file and
/// any file longer than `limit` bytes.
///
/// `limit` itself must not exceed [`MAX_ARCHIVE_BYTES`]. The path is inspected
/// with [`fs::metadata`] first (which never opens it), so a FIFO, socket or
/// directory is rejected instead of blocking in `open(2)`. Type and length are
/// then re-checked on the opened handle, so a path swapped between the two
/// calls is rejected as well.
///
/// Returns a [`File`] positioned at the start of the file; callers that only
/// need the header (the catalog probe) can read exactly that many bytes.
pub fn open_file_bounded(path: &Path, limit: u64) -> Result<File, StorageError> {
    reject_over_limit(limit, MAX_ARCHIVE_BYTES)?;

    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(StorageError::NotRegularFile);
    }
    reject_over_limit(metadata.len(), limit)?;

    let file = File::open(path)?;
    // Recheck type and length from the opened handle: the path can change
    // between metadata and open (benign TOCTOU).
    let opened = file.metadata()?;
    if !opened.is_file() {
        return Err(StorageError::NotRegularFile);
    }
    reject_over_limit(opened.len(), limit)?;
    Ok(file)
}

/// Reads all of `path`, refusing to allocate more than `limit` bytes.
///
/// Reads through `take(limit + 1)` so a file that grew between the metadata
/// check and the read is rejected as [`StorageError::TooLarge`] rather than
/// silently truncated. `limit + 1` cannot overflow because `limit` is
/// validated against [`MAX_ARCHIVE_BYTES`] first.
pub fn read_file_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, StorageError> {
    reject_over_limit(limit, MAX_ARCHIVE_BYTES)?;

    let file = open_file_bounded(path, limit)?;
    let mut reader = file.take(limit + 1);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    // A file that grew between the metadata check and the read is still
    // rejected here rather than silently truncated.
    reject_over_limit(u64::try_from(bytes.len()).unwrap_or(u64::MAX), limit)?;
    Ok(bytes)
}

/// Writes `bytes` to `path` atomically via a sibling `.mcvx.tmp` file.
///
/// The parent directory is created when it is non-empty. `bytes` longer than
/// [`MAX_ARCHIVE_BYTES`] are rejected before the directory, temp file or
/// target is touched. The temp file is flushed and synced before
/// [`fs::rename`] replaces the target, so a failed or interrupted write
/// leaves the previous save in place.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    reject_over_limit(
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        MAX_ARCHIVE_BYTES,
    )?;

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let tmp_path = path.with_extension("mcvx.tmp");
    {
        let file = File::create(&tmp_path)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(bytes)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::io::Seek;
    use std::io::SeekFrom;

    #[test]
    fn read_file_bounded_round_trips_and_honours_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("world.mcvx");
        fs::write(&path, b"exact bytes").unwrap();

        let bytes = read_file_bounded(&path, 11).expect("exact fit must read");
        assert_eq!(bytes, b"exact bytes");

        let err = read_file_bounded(&path, 10).expect_err("short limit must reject");
        assert!(
            matches!(err, StorageError::TooLarge { len: 11, limit: 10 }),
            "got {err:?}"
        );

        let empty = dir.path().join("empty.mcvx");
        fs::write(&empty, b"").unwrap();
        assert_eq!(read_file_bounded(&empty, 0).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn open_file_bounded_rejects_a_limit_above_the_crate_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("world.mcvx");

        // The limit is validated before the path is even touched, so a
        // missing file still reports the cap, not a missing-file I/O error.
        let err = open_file_bounded(&path, MAX_ARCHIVE_BYTES + 1).unwrap_err();
        assert!(
            matches!(
                err,
                StorageError::TooLarge { len, limit }
                    if len == MAX_ARCHIVE_BYTES + 1 && limit == MAX_ARCHIVE_BYTES
            ),
            "got {err:?}"
        );

        let err = read_file_bounded(&path, MAX_ARCHIVE_BYTES + 1).unwrap_err();
        assert!(
            matches!(err, StorageError::TooLarge { limit, .. } if limit == MAX_ARCHIVE_BYTES),
            "got {err:?}"
        );
    }

    #[test]
    fn oversized_sparse_file_is_rejected_before_it_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.mcvx");
        // Sparse on ext4/NTFS: only the length is set, the body is never
        // written and never read by either entry point below.
        let file = File::create(&path).unwrap();
        file.set_len(65).unwrap();
        drop(file);

        let err = open_file_bounded(&path, 64).expect_err("open must reject");
        assert!(
            matches!(err, StorageError::TooLarge { len: 65, limit: 64 }),
            "got {err:?}"
        );

        let err = read_file_bounded(&path, 64).expect_err("read must reject");
        assert!(
            matches!(err, StorageError::TooLarge { len: 65, limit: 64 }),
            "got {err:?}"
        );
    }

    #[test]
    fn directory_at_save_path_is_rejected_as_not_regular() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.mcvx");
        fs::create_dir(&path).unwrap();

        for err in [
            open_file_bounded(&path, 1024).unwrap_err(),
            read_file_bounded(&path, 1024).unwrap_err(),
        ] {
            assert!(
                matches!(err, StorageError::NotRegularFile),
                "expected NotRegularFile, got {err:?}"
            );
        }
        assert!(StorageError::NotRegularFile
            .to_string()
            .contains("not a regular file"));
    }

    #[cfg(unix)]
    #[test]
    fn fifo_and_socket_save_paths_are_rejected_without_blocking() {
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();

        // A Unix socket is not a regular file and std can create it with no
        // external binary, so this leg always runs.
        let socket = dir.path().join("sock.mcvx");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        for err in [
            open_file_bounded(&socket, 1024).unwrap_err(),
            read_file_bounded(&socket, 1024).unwrap_err(),
        ] {
            assert!(matches!(err, StorageError::NotRegularFile), "got {err:?}");
        }
        drop(listener);

        // A FIFO is the hazard the guard exists for: open(2) for reading
        // blocks until a writer appears. mkfifo(1) is the only dependency-
        // free way to create one (std has no mkfifo).
        let fifo = dir.path().join("pipe.mcvx");
        match std::process::Command::new("mkfifo").arg(&fifo).status() {
            Ok(status) if status.success() => {}
            other => {
                eprintln!("skipping FIFO leg: mkfifo unavailable ({other:?})");
                return;
            }
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let probe_path = fifo.clone();
        let reader = std::thread::spawn(move || {
            let result = open_file_bounded(&probe_path, 1024);
            let _ = tx.send(format!("{result:?}"));
        });
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(payload) => {
                reader.join().unwrap();
                assert!(payload.contains("NotRegularFile"), "got {payload}");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // The reader is stuck in open(2); a writer open completes the
                // pair so the thread can exit, then we fail the test.
                let _ = File::create(&fifo);
                reader.join().unwrap();
                panic!("open_file_bounded blocked on a FIFO before rejecting it");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // The reader panicked; do NOT open the FIFO for writing here
                // (there would be no reader to pair with and this test would
                // hang). Surface the thread panic instead.
                reader.join().unwrap();
                panic!("reader thread disconnected before reporting");
            }
        }
    }

    #[test]
    fn open_file_bounded_can_read_only_the_eight_byte_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("world.mcvx");
        let mut body = b"MCVX".to_vec();
        body.extend_from_slice(&9u32.to_le_bytes());
        body.extend_from_slice(b"a body the probe must not read");

        fs::write(&path, &body).unwrap();

        let file = open_file_bounded(&path, MAX_ARCHIVE_BYTES).unwrap();
        let mut header = [0u8; 8];
        file.take(8).read_exact(&mut header).unwrap();
        assert_eq!(&header[..4], b"MCVX");
        assert_eq!(u32::from_le_bytes(header[4..].try_into().unwrap()), 9);
    }

    #[test]
    fn atomic_overwrite_leaves_the_new_bytes_and_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("world.mcvx");
        fs::write(&path, b"old save bytes").unwrap();

        write_atomic(&path, b"new save bytes").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new save bytes");
        assert!(
            !path.with_extension("mcvx.tmp").exists(),
            "the temp file must be renamed away, not left behind"
        );
        let mut names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, vec![std::ffi::OsString::from("world.mcvx")]);
    }

    #[test]
    fn atomic_write_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deeper/world.mcvx");

        write_atomic(&path, b"payload").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"payload");
    }

    #[test]
    fn failed_atomic_write_never_replaces_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("world.mcvx");
        fs::write(&path, b"original").unwrap();
        // Occupy the temp path with a directory so File::create fails before
        // anything is written.
        fs::create_dir(path.with_extension("mcvx.tmp")).unwrap();

        let err = write_atomic(&path, b"replacement").expect_err("blocked temp must fail");
        assert!(matches!(err, StorageError::Io(_)), "got {err:?}");
        assert_eq!(
            fs::read(&path).unwrap(),
            b"original",
            "a failed write must leave the previous save intact"
        );
    }

    #[test]
    fn write_over_the_crate_cap_is_rejected_without_creating_anything() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deeper/huge.mcvx");
        // `vec![0u8; n]` is zero-initialised lazily; only the length is
        // inspected before the write is rejected.
        let bytes = vec![0u8; usize::try_from(MAX_ARCHIVE_BYTES).unwrap() + 1];

        let err = write_atomic(&path, &bytes).expect_err("over-cap write must reject");
        assert!(
            matches!(
                err,
                StorageError::TooLarge { len, limit }
                    if len == MAX_ARCHIVE_BYTES + 1 && limit == MAX_ARCHIVE_BYTES
            ),
            "got {err:?}"
        );
        assert!(!path.exists(), "the target must not be created");
        assert!(
            !path.with_extension("mcvx.tmp").exists(),
            "the temp file must not be created"
        );
        assert!(
            !dir.path().join("nested").exists(),
            "the parent directory must not be created for a rejected write"
        );
    }

    #[test]
    fn open_file_bounded_leaves_the_position_at_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("world.mcvx");
        fs::write(&path, b"0123456789").unwrap();

        let mut file = open_file_bounded(&path, 10).unwrap();
        let mut first = [0u8; 2];
        file.read_exact(&mut first).unwrap();
        assert_eq!(&first, b"01");
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut all = Vec::new();
        file.read_to_end(&mut all).unwrap();
        assert_eq!(all, b"0123456789");
    }
}
