//! The file operations the journal is built from, with each OS's durability rules in one place.

use crate::StorageError;
use std::{
    fs::{self, File},
    io::{self, Write},
    path::Path,
};

pub(super) fn err(what: &str, path: &Path) -> impl FnOnce(io::Error) -> StorageError + use<> {
    let context = format!("could not {what} {}", path.display());
    move |source| StorageError::io(context, source)
}

/// Writes a new file and flushes it to the device before returning.
pub(super) fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    let mut file = File::create_new(path).map_err(err("create", path))?;
    file.write_all(bytes).map_err(err("write", path))?;
    file.sync_all().map_err(err("flush", path))
}

/// A new name for an existing file: a hard link, so nothing is copied, or a copy flushed to the
/// device where the filesystem has no hard links (FAT, some network shares). Blob files are never
/// changed in place, so sharing one between an old and a new record is safe.
pub(super) fn link_or_copy(from: &Path, to: &Path) -> Result<(), StorageError> {
    if fs::hard_link(from, to).is_ok() {
        return Ok(());
    }
    fs::copy(from, to).map_err(err("copy", from))?;
    File::open(to)
        .and_then(|file| file.sync_all())
        .map_err(err("flush", to))
}

/// Makes the entries of `dir` (creations, renames, removals) durable.
///
/// Unix: `fsync` on the directory; on macOS Rust's `sync_all` is `F_FULLFSYNC`, which also flushes
/// the drive's cache. A filesystem that cannot sync a directory says so with `EINVAL`, and then
/// there is nothing more to do. Windows has no directory sync: NTFS logs metadata changes in
/// order, so a rename is never half done, but the last ones before a power cut may be undone.
#[cfg(unix)]
pub(super) fn sync_dir(dir: &Path) -> Result<(), StorageError> {
    match File::open(dir).and_then(|dir| dir.sync_all()) {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
            ) =>
        {
            Ok(())
        }
        result => result.map_err(err("flush the directory", dir)),
    }
}

#[cfg(not(unix))]
pub(super) fn sync_dir(_dir: &Path) -> Result<(), StorageError> {
    Ok(())
}

pub(super) fn rename(from: &Path, to: &Path) -> Result<(), StorageError> {
    retrying(|| fs::rename(from, to)).map_err(err("move", from))
}

/// Removes a directory tree; one that is already gone is fine.
pub(super) fn remove_dir_all(dir: &Path) -> Result<(), StorageError> {
    match retrying(|| fs::remove_dir_all(dir)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result.map_err(err("remove", dir)),
    }
}

/// Removes a file; one that is already gone is fine.
pub(super) fn remove_file(path: &Path) -> Result<(), StorageError> {
    match retrying(|| fs::remove_file(path)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result.map_err(err("remove", path)),
    }
}

pub(super) fn is_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// Retries an operation another process can block for a moment. On Windows a virus scanner,
/// the search indexer or a backup tool briefly opening a file makes renaming or removing it, or a
/// directory holding it, fail with a sharing or access error. Elsewhere it runs once.
fn retrying<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    if cfg!(windows) {
        let mut delay = std::time::Duration::from_millis(5);
        for _ in 0..8 {
            match op() {
                Err(error) if is_transient(&error) => std::thread::sleep(delay),
                result => return result,
            }
            delay *= 2;
        }
    }
    op()
}

/// `ERROR_ACCESS_DENIED`, `ERROR_SHARING_VIOLATION` and `ERROR_LOCK_VIOLATION`.
fn is_transient(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(5 | 32 | 33))
}
