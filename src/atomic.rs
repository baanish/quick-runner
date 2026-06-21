//! Atomic file writes: write to a temp file in the same directory, fsync, then
//! rename it over the destination. A concurrent reader sees either the old file
//! or the fully-written new one — never a truncated/partial file — and an
//! interrupted write (crash, full disk, SIGKILL) cannot corrupt the destination.
//!
//! This matters for files `qr` does not exclusively own or that are read
//! concurrently: the project cache (read by `qr go` while the hourly cron
//! rewrites it) and the user's shell rc file (corrupting it is a 5-alarm fire).

use std::{
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

/// Atomically replace `path` with `contents`.
pub fn write(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }

    let tmp = temp_path(path);
    let mut file = fs::File::create(&tmp)
        .with_context(|| format!("Failed to create temp file {}", tmp.display()))?;
    let result = file.write_all(contents).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = result {
        let _ = fs::remove_file(&tmp);
        return Err(error).with_context(|| format!("Failed to write {}", tmp.display()));
    }

    // Preserve the existing file's permissions across the replace: rename
    // installs a fresh inode, so without this an existing rc/config file's mode
    // (e.g. a 0600 config) would reset to the default.
    if let Ok(metadata) = fs::metadata(path) {
        let _ = fs::set_permissions(&tmp, metadata.permissions());
    }

    if let Err(error) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(error).with_context(|| format!("Failed to replace {}", path.display()));
    }
    Ok(())
}

/// A temp path in the same directory as `path` (so `rename` stays on one
/// filesystem and is atomic), made unique by PID to avoid collisions between
/// concurrent writers.
fn temp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| OsString::from("qr-tmp"));
    name.push(format!(".tmp.{}", std::process::id()));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_creates_parent_replaces_existing_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/data.json");

        write(&path, b"first").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"first");

        // Overwriting an existing file must succeed and fully replace it.
        write(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");

        let temps: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(temps.is_empty(), "left a temp file behind: {temps:?}");
    }

    #[cfg(unix)]
    #[test]
    fn write_preserves_existing_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write(&path, b"v1").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        write(&path, b"v2").unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "atomic replace must preserve the file mode");
    }
}
