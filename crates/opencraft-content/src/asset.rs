//! Pack asset path confinement and byte caps.
//!
//! v2 packs author asset paths as relative strings. Before any file is read,
//! the path must be proven to stay inside the pack folder: absolute paths,
//! `..`, `.`-only names and symlink escapes are rejected. Every rejection is a
//! per-entry error string that the caller turns into a diagnostic, never a
//! pack failure.

use std::path::{Component, Path, PathBuf};

/// Largest accepted single asset file, in bytes.
pub const PACK_ASSET_BYTE_CAP: u64 = 1_048_576;

/// Largest accepted cumulative raw asset payload for one pack, in bytes.
pub const PACK_TOTAL_ASSET_BYTES_CAP: u64 = 8 * 1_048_576;

/// Validate that `relative` is a plain relative asset path.
///
/// This is the pure (filesystem-free) half of the confinement check, exposed
/// so the TOML parser can reject an authored path before touching the disk.
pub fn validate_relative_asset_path(relative: &str) -> Result<(), String> {
    if relative.is_empty() {
        return Err("asset path is empty".to_string());
    }

    let path = Path::new(relative);
    if path.is_absolute() {
        return Err(format!(
            "asset path `{relative}` must be relative to the pack folder"
        ));
    }

    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir => {
                return Err(format!(
                    "asset path `{relative}` must not contain `.` components"
                ));
            }
            Component::ParentDir => {
                return Err(format!(
                    "asset path `{relative}` must not contain `..` components"
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "asset path `{relative}` must be relative to the pack folder"
                ));
            }
        }
    }

    Ok(())
}

/// Resolve `relative` to a path inside `pack_dir`, rejecting escapes.
///
/// A path that does not exist yet is returned lexically so the caller's
/// missing-file diagnostic still names the authored path. An existing path is
/// canonicalized (which resolves symlinks) and must stay under the
/// canonicalized pack folder.
pub fn resolve_asset_path(pack_dir: &Path, relative: &str) -> Result<PathBuf, String> {
    validate_relative_asset_path(relative)?;
    let candidate = pack_dir.join(relative);

    let Ok(_) = std::fs::symlink_metadata(&candidate) else {
        // Missing (or an unresolved ancestor): keep the lexical path so the
        // eventual read produces a per-entry missing-file diagnostic.
        return Ok(candidate);
    };

    let canonical_dir = std::fs::canonicalize(pack_dir).map_err(|err| {
        format!(
            "failed to resolve pack folder {} while checking `{relative}`: {err}",
            pack_dir.display()
        )
    })?;
    let canonical = std::fs::canonicalize(&candidate)
        .map_err(|err| format!("failed to resolve asset `{relative}`: {err}"))?;

    if !canonical.starts_with(&canonical_dir) {
        return Err(format!("asset path `{relative}` escapes the pack folder"));
    }

    Ok(canonical)
}

/// Read one confined asset file, enforcing the per-file byte cap twice.
///
/// The metadata length is checked before reading so a huge file is never
/// buffered, and the actual read length is re-checked afterwards for sparse or
/// changing files.
pub fn read_asset_bytes(pack_dir: &Path, relative: &str) -> Result<(PathBuf, Vec<u8>), String> {
    let path = resolve_asset_path(pack_dir, relative)?;

    let metadata = std::fs::metadata(&path)
        .map_err(|err| format!("failed to read asset `{relative}`: {err}"))?;
    if !metadata.is_file() {
        return Err(format!("asset `{relative}` is not a regular file"));
    }
    if metadata.len() > PACK_ASSET_BYTE_CAP {
        return Err(format!(
            "asset `{relative}` is {} bytes, larger than the {} byte per-file limit",
            metadata.len(),
            PACK_ASSET_BYTE_CAP
        ));
    }

    let bytes =
        std::fs::read(&path).map_err(|err| format!("failed to read asset `{relative}`: {err}"))?;
    if bytes.len() as u64 > PACK_ASSET_BYTE_CAP {
        return Err(format!(
            "asset `{relative}` is {} bytes, larger than the {} byte per-file limit",
            bytes.len(),
            PACK_ASSET_BYTE_CAP
        ));
    }

    Ok((path, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn lexical_matrix_rejects_escapes_and_accepts_plain_paths() {
        for valid in ["ruby.png", "textures/ruby.png", "a/b/c.png"] {
            assert!(
                validate_relative_asset_path(valid).is_ok(),
                "`{valid}` should be a valid relative asset path"
            );
        }

        for invalid in [
            "",
            ".",
            "./ruby.png",
            "..",
            "../ruby.png",
            "textures/../../ruby.png",
            "/etc/passwd",
            "textures//../ruby.png",
        ] {
            assert!(
                validate_relative_asset_path(invalid).is_err(),
                "`{invalid}` should be rejected"
            );
        }
    }

    #[test]
    fn missing_asset_keeps_lexical_path() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_asset_path(dir.path(), "textures/missing.png").unwrap();
        assert_eq!(resolved, dir.path().join("textures/missing.png"));
    }

    #[test]
    fn existing_asset_resolves_inside_pack() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ruby.png");
        fs::write(&path, b"png").unwrap();

        let resolved = resolve_asset_path(dir.path(), "ruby.png").unwrap();
        assert_eq!(resolved, fs::canonicalize(&path).unwrap());
    }

    #[test]
    fn absolute_path_is_rejected_before_filesystem_access() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_asset_path(dir.path(), "/etc/passwd").unwrap_err();
        assert!(err.contains("relative"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let pack = root.path().join("pack");
        fs::create_dir_all(&pack).unwrap();
        let outside = root.path().join("outside.png");
        fs::write(&outside, b"secret").unwrap();
        symlink(&outside, pack.join("escape.png")).unwrap();

        let err = resolve_asset_path(&pack, "escape.png").unwrap_err();
        assert!(err.contains("escapes"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directory_escape_is_rejected() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let pack = root.path().join("pack");
        let outside = root.path().join("outside");
        fs::create_dir_all(&pack).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("asset.png"), b"secret").unwrap();
        symlink(&outside, pack.join("linked")).unwrap();

        let err = resolve_asset_path(&pack, "linked/asset.png").unwrap_err();
        assert!(err.contains("escapes"), "{err}");
    }

    #[test]
    fn read_asset_returns_bytes_and_path() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("ruby.png"), b"hello").unwrap();

        let (path, bytes) = read_asset_bytes(dir.path(), "ruby.png").unwrap();
        assert_eq!(bytes, b"hello");
        assert!(path.ends_with("ruby.png"));
    }

    #[test]
    fn read_asset_rejects_directory() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("assets")).unwrap();

        let err = read_asset_bytes(dir.path(), "assets").unwrap_err();
        assert!(err.contains("regular file"), "{err}");
    }

    #[test]
    fn per_file_cap_is_enforced_before_decode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.bin");
        fs::write(&path, vec![0u8; PACK_ASSET_BYTE_CAP as usize + 1]).unwrap();

        let err = read_asset_bytes(dir.path(), "huge.bin").unwrap_err();
        assert!(err.contains("per-file limit"), "{err}");
    }
}
