//! A [`Vfs`] over a physical asset root with its archives mounted, for tooling.

use crate::{BoxedFuture, DirEntry, Mounts, StdVfs, Vfs, VfsError};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

/// [`StdVfs`] with every `.zip`/`.tar` under the root read as a directory. Blocking, like
/// [`StdVfs`]: it is for command line tools and tests.
pub struct ArchiveVfs {
    loose: StdVfs,
    mounts: Arc<Mounts>,
}

impl ArchiveVfs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            loose: StdVfs::new(root.clone()),
            mounts: Arc::new(Mounts::new(root)),
        }
    }

    pub fn mounts(&self) -> &Arc<Mounts> {
        &self.mounts
    }
}

impl Vfs for ArchiveVfs {
    fn read_file<'a>(&'a self, path: &'a Path) -> BoxedFuture<'a, Result<Vec<u8>, VfsError>> {
        Box::pin(async move {
            match self.mounts.read_file(path) {
                Some(bytes) => bytes.map_err(|err| vfs_error(path, err)),
                None => self.loose.read_file(path).await,
            }
        })
    }

    fn read_dir<'a>(&'a self, path: &'a Path) -> BoxedFuture<'a, Result<Vec<DirEntry>, VfsError>> {
        Box::pin(async move {
            if let Some(entries) = self.mounts.read_dir(path) {
                return entries.map_err(|err| vfs_error(path, err));
            }
            let entries = self.loose.read_dir(path).await?;
            let is_dir: HashMap<&Path, bool> = entries
                .iter()
                .map(|entry| (entry.path.as_path(), entry.is_dir))
                .collect();
            let paths: Vec<PathBuf> = entries.iter().map(|entry| entry.path.clone()).collect();
            let Some(overlaid) = self.mounts.overlay_listing(path, &paths, true) else {
                return Ok(entries);
            };
            Ok(overlaid
                .into_iter()
                .map(|(path, mount)| DirEntry {
                    is_dir: mount || is_dir.get(path.as_path()).copied().unwrap_or(false),
                    path,
                })
                .collect())
        })
    }
}

fn vfs_error(path: &Path, err: io::Error) -> VfsError {
    match err.kind() {
        io::ErrorKind::NotFound => VfsError::NotFound(path.to_path_buf()),
        _ => VfsError::Io {
            path: path.to_path_buf(),
            message: err.to_string(),
        },
    }
}
