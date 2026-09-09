//! The filesystem seam.
//!
//! The scanner reads through a [`Vfs`] and never through `std::fs` or a Bevy type directly. Two
//! implementations exist for one reason: the running game reads through Bevy's `AssetReader`, so
//! wasm over HTTP and Android's APK work with no second code path, and a command line tool reads
//! through `std::fs` with no Bevy in its dependency graph at all. One walk over one trait is what
//! stops the two from drifting.
//!
//! The trait names its future type instead of using `async fn` in a trait. That buys two things
//! the opaque form cannot: a `Send` bound, which the scan needs because it is spawned on
//! `IoTaskPool`, and dyn-safety, which is what lets the walk take `&dyn Vfs` and be compiled once.

use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
};
use thiserror::Error;

/// A boxed, `Send` future. Spelled out here rather than borrowed from Bevy, because this crate
/// compiles without Bevy.
pub type BoxedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Why a [`Vfs`] call failed.
///
/// `NotFound` is separate because the scanner treats it as an answer rather than an error: a
/// directory with no `manifest.toml` is not a package, and a search path that does not exist is a
/// warning and not a failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VfsError {
    #[error("not found: {0}")]
    NotFound(PathBuf),
    #[error("{path}: {message}")]
    Io { path: PathBuf, message: String },
}

/// One entry in a directory listing.
///
/// `is_dir` is carried here rather than left to a second call, because `std::fs` hands it over for
/// free and asking again per entry over a network reader would not be.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DirEntry {
    /// Relative to the vfs root, not to the directory that was listed.
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Reading a tree of files. Every path is relative to the implementation's root.
///
/// Both implementations must agree on what a listing contains, or a tool and the game disagree
/// about what is in a package. Two rules, matching what Bevy's file reader already does:
/// `*.meta` is never listed, and neither is anything whose name starts with `.`.
/// Entries come back sorted by path, because directory name is a tiebreaker the whole load order
/// rests on and the platform's order is not stable.
pub trait Vfs: Send + Sync {
    fn read_file<'a>(&'a self, path: &'a Path) -> BoxedFuture<'a, Result<Vec<u8>, VfsError>>;

    fn read_dir<'a>(&'a self, path: &'a Path) -> BoxedFuture<'a, Result<Vec<DirEntry>, VfsError>>;
}

/// True for an entry a listing must hide: a `.meta` sidecar or a dot-file.
pub(crate) fn is_hidden_from_listings(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    name.starts_with('.')
        || path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("meta"))
}

/// A [`Vfs`] over `std::fs`, rooted at a directory.
///
/// The io is blocking inside an async signature. That is deliberate and it is only used by tooling
/// and tests: a command line scan is the whole program, so there is nothing for it to block.
/// The game uses [`AssetReaderVfs`], whose io is genuinely async.
pub struct StdVfs {
    root: PathBuf,
}

impl StdVfs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl Vfs for StdVfs {
    fn read_file<'a>(&'a self, path: &'a Path) -> BoxedFuture<'a, Result<Vec<u8>, VfsError>> {
        Box::pin(async move {
            let full = self.root.join(path);
            std::fs::read(&full).map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => VfsError::NotFound(path.to_path_buf()),
                _ => VfsError::Io {
                    path: path.to_path_buf(),
                    message: err.to_string(),
                },
            })
        })
    }

    fn read_dir<'a>(&'a self, path: &'a Path) -> BoxedFuture<'a, Result<Vec<DirEntry>, VfsError>> {
        Box::pin(async move {
            let full = self.root.join(path);
            let read_dir = std::fs::read_dir(&full).map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => VfsError::NotFound(path.to_path_buf()),
                _ => VfsError::Io {
                    path: path.to_path_buf(),
                    message: err.to_string(),
                },
            })?;

            let mut entries = Vec::new();
            for entry in read_dir.flatten() {
                let relative = path.join(entry.file_name());
                if is_hidden_from_listings(&relative) {
                    continue;
                }
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                entries.push(DirEntry {
                    path: relative,
                    is_dir,
                });
            }
            entries.sort();
            Ok(entries)
        })
    }
}

/// A [`Vfs`] over one of Bevy's asset readers.
///
/// Build it from `AssetSource::get_default_reader`. Never from the `alias://` source's reader:
/// that would await an index only this scan can fill, and hang.
#[cfg(feature = "bevy")]
pub struct AssetReaderVfs {
    reader: Box<dyn bevy::asset::io::ErasedAssetReader>,
}

#[cfg(feature = "bevy")]
impl AssetReaderVfs {
    pub fn new(reader: Box<dyn bevy::asset::io::ErasedAssetReader>) -> Self {
        Self { reader }
    }
}

#[cfg(feature = "bevy")]
impl Vfs for AssetReaderVfs {
    fn read_file<'a>(&'a self, path: &'a Path) -> BoxedFuture<'a, Result<Vec<u8>, VfsError>> {
        use bevy::tasks::futures_lite::AsyncReadExt;
        Box::pin(async move {
            let mut reader = self
                .reader
                .read(path)
                .await
                .map_err(|err| vfs_error(path, err))?;
            let mut bytes = Vec::new();
            reader
                .read_to_end(&mut bytes)
                .await
                .map_err(|err| VfsError::Io {
                    path: path.to_path_buf(),
                    message: err.to_string(),
                })?;
            Ok(bytes)
        })
    }

    fn read_dir<'a>(&'a self, path: &'a Path) -> BoxedFuture<'a, Result<Vec<DirEntry>, VfsError>> {
        use bevy::tasks::futures_lite::StreamExt;
        Box::pin(async move {
            let mut stream = self
                .reader
                .read_directory(path)
                .await
                .map_err(|err| vfs_error(path, err))?;

            let mut entries = Vec::new();
            while let Some(entry) = stream.next().await {
                // Bevy's file reader already drops `.meta` and dot-files. Other readers are not
                // required to, and both implementations of this trait have to agree.
                if is_hidden_from_listings(&entry) {
                    continue;
                }
                let is_dir = matches!(self.reader.is_directory(&entry).await, Ok(true));
                entries.push(DirEntry {
                    path: entry,
                    is_dir,
                });
            }
            entries.sort();
            Ok(entries)
        })
    }
}

#[cfg(feature = "bevy")]
fn vfs_error(path: &Path, err: bevy::asset::io::AssetReaderError) -> VfsError {
    match err {
        bevy::asset::io::AssetReaderError::NotFound(_) => VfsError::NotFound(path.to_path_buf()),
        other => VfsError::Io {
            path: path.to_path_buf(),
            message: other.to_string(),
        },
    }
}
