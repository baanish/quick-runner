//! Atomic file writes: write to a temp file in the same directory, fsync, then
//! rename it over the destination. A concurrent reader sees either the old file
//! or the fully-written new one — never a truncated/partial file — and an
//! interrupted write (crash, full disk, SIGKILL) cannot corrupt the destination.
//!
//! This matters for files `qr` does not exclusively own or that are read
//! concurrently: the project cache (read by `qr go` while the hourly cron
//! rewrites it) and the user's shell rc file (corrupting it is a 5-alarm fire).

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};

/// Atomically replace `path` with `contents`.
pub fn write(path: &Path, contents: &[u8]) -> Result<()> {
    write_impl(path, contents, WriteMode::PreserveExisting)
}

/// Atomically replace `path` with `contents`, creating the file with private
/// permissions (`0600`) from the start on Unix.
pub fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    write_impl(path, contents, WriteMode::Private)
}

enum WriteMode {
    PreserveExisting,
    Private,
}

fn write_impl(path: &Path, contents: &[u8], mode: WriteMode) -> Result<()> {
    // Follow a final symlink so we replace its target — as `fs::write` did —
    // rather than clobbering the symlink itself with a regular file. This keeps
    // dotfiles-managed symlinked rc files (e.g. `~/.zshrc` -> a versioned repo)
    // intact.
    let target = resolve_symlink(path);
    let target = target.as_path();

    if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }

    let tmp = temp_path(target);
    let mut file = create_temp_file(&tmp, &mode)?;
    let result = file.write_all(contents).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = result {
        let _ = fs::remove_file(&tmp);
        return Err(error).with_context(|| format!("Failed to write {}", tmp.display()));
    }

    // Preserve the existing file's permissions across the replace: rename
    // installs a fresh inode, so without this an existing rc/config file's mode
    // (e.g. a 0600 config) would reset to the default. Fail loudly rather than
    // silently downgrade the mode.
    match mode {
        WriteMode::PreserveExisting => {
            if let Ok(metadata) = fs::metadata(target) {
                if let Err(error) = fs::set_permissions(&tmp, metadata.permissions()) {
                    let _ = fs::remove_file(&tmp);
                    return Err(error).with_context(|| {
                        format!("Failed to preserve permissions on {}", target.display())
                    });
                }
            }
        }
        WriteMode::Private => {
            #[cfg(unix)]
            if let Err(error) = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600)) {
                let _ = fs::remove_file(&tmp);
                return Err(error)
                    .with_context(|| format!("Failed to secure {}", target.display()));
            }
        }
    }

    if let Err(error) = fs::rename(&tmp, target) {
        let _ = fs::remove_file(&tmp);
        return Err(error).with_context(|| format!("Failed to replace {}", target.display()));
    }
    Ok(())
}

fn create_temp_file(path: &Path, mode: &WriteMode) -> Result<fs::File> {
    #[cfg(unix)]
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        if matches!(mode, WriteMode::Private) {
            options.mode(0o600);
        }
        options
            .open(path)
            .with_context(|| format!("Failed to create temp file {}", path.display()))
    }

    #[cfg(not(unix))]
    {
        let _ = mode;
        fs::File::create(path)
            .with_context(|| format!("Failed to create temp file {}", path.display()))
    }
}

/// Resolve a final symlink to its real target so an atomic replacement writes
/// through the link (matching `fs::write`) instead of clobbering it — including a
/// dangling symlink, whose not-yet-existing target is created rather than the
/// link being replaced by a regular file (a dotfiles-managed rc symlink survives
/// even if its target is temporarily missing).
///
/// Deliberately NOT handled, because neither is reachable for the single-user
/// rc/config files qr writes: a symlink *loop* (resolves to one link, which is
/// then replaced) and a target *swapped in after this check* (TOCTOU) — the
/// latter also can't be closed without an `O_NOFOLLOW` open/rename dance.
fn resolve_symlink(path: &Path) -> PathBuf {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            // Live symlink: canonicalize resolves the whole chain to the target.
            if let Ok(real) = fs::canonicalize(path) {
                return real;
            }
            // Dangling symlink: canonicalize fails because the target is missing,
            // so resolve one level via read_link and write through to it.
            match fs::read_link(path) {
                Ok(target) if target.is_absolute() => target,
                Ok(target) => path
                    .parent()
                    .map(|parent| parent.join(target))
                    .unwrap_or_else(|| path.to_path_buf()),
                Err(_) => path.to_path_buf(),
            }
        }
        _ => path.to_path_buf(),
    }
}

/// A temp path in the same directory as `path` (so `rename` stays on one
/// filesystem and is atomic), made unique by PID *and* a per-process atomic
/// counter. The counter matters because two threads writing the same target
/// (e.g. the test suite, or any future concurrent caller) would otherwise share
/// `<name>.tmp.<pid>` and clobber each other's temp file mid-write.
fn temp_path(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = path
        .file_name()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| OsString::from("qr-tmp"));
    name.push(format!(".tmp.{}.{unique}", std::process::id()));
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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write(&path, b"v1").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        write(&path, b"v2").unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "atomic replace must preserve the file mode");
    }

    #[cfg(unix)]
    #[test]
    fn write_private_creates_file_with_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        write_private(&path, b"secret = true").unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "private writes must create 0600 files");
    }

    #[test]
    fn concurrent_writes_to_same_path_never_interleave() {
        // Two threads writing the same target must each use a private temp file,
        // so the destination is always one writer's complete buffer — never a
        // mix of both, and never a leftover temp.
        use std::thread;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        let buffers: Vec<Vec<u8>> = (0..6u8).map(|i| vec![b'a' + i; 8192]).collect();

        let handles: Vec<_> = buffers
            .iter()
            .map(|buf| {
                let buf = buf.clone();
                let path = path.clone();
                thread::spawn(move || {
                    for _ in 0..25 {
                        write(&path, &buf).unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let final_bytes = fs::read(&path).unwrap();
        assert!(
            buffers.contains(&final_bytes),
            "destination is not any single writer's complete buffer"
        );
        let temps: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(temps.is_empty(), "left temp files behind: {temps:?}");
    }

    #[cfg(unix)]
    #[test]
    fn write_follows_symlink_instead_of_clobbering_it() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real_rc");
        let link = dir.path().join("link_rc");
        write(&real, b"v1").unwrap();
        symlink(&real, &link).unwrap();

        write(&link, b"v2").unwrap();

        // The symlink must survive (a dotfiles symlink, e.g. ~/.zshrc, stays a
        // symlink) and the write must land on the real target.
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "symlink was replaced by a regular file"
        );
        assert_eq!(fs::read(&real).unwrap(), b"v2");
        assert_eq!(fs::read(&link).unwrap(), b"v2");
    }

    #[cfg(unix)]
    #[test]
    fn write_through_a_dangling_symlink_creates_the_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("missing_target");
        let link = dir.path().join("link_rc");
        // A dangling symlink: the target does not exist yet (e.g. a dotfiles
        // symlink whose repo file is temporarily missing).
        symlink(&target, &link).unwrap();

        write(&link, b"v1").unwrap();

        // The link must survive (not be replaced by a regular file) and the write
        // must create and land on its target — matching `fs::write`.
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "dangling symlink was clobbered by a regular file"
        );
        assert_eq!(fs::read(&target).unwrap(), b"v1");
        assert_eq!(fs::read(&link).unwrap(), b"v1");
    }
}
