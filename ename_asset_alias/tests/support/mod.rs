//! A `Vfs` backed by a map, so a discovery test can put its whole tree in the test body.
//!
//! Reading the tree next to the assertion beats reading five fixture files, and it is the only way
//! to test an unreadable directory at all. `alias_scan.rs` here and `package_scan.rs` in
//! `ename_asset_package` both drive real trees, so `StdVfs` and `AssetReaderVfs` stay exercised
//! too.

use ename_asset_alias::{BoxedFuture, DirEntry, Vfs, VfsError};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Default)]
pub struct FakeVfs {
    files: BTreeMap<PathBuf, Vec<u8>>,
    unreadable: BTreeSet<PathBuf>,
}

impl FakeVfs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a file. Intermediate directories are implied by the path.
    pub fn file(mut self, path: &str, contents: &str) -> Self {
        self.files.insert(PathBuf::from(path), contents.into());
        self
    }

    /// Makes one directory fail to list, which a real fixture tree cannot express portably.
    pub fn unreadable_dir(mut self, path: &str) -> Self {
        self.unreadable.insert(PathBuf::from(path));
        self
    }
}

impl Vfs for FakeVfs {
    fn read_file<'a>(&'a self, path: &'a Path) -> BoxedFuture<'a, Result<Vec<u8>, VfsError>> {
        Box::pin(async move {
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| VfsError::NotFound(path.to_path_buf()))
        })
    }

    fn read_dir<'a>(&'a self, path: &'a Path) -> BoxedFuture<'a, Result<Vec<DirEntry>, VfsError>> {
        Box::pin(async move {
            if self.unreadable.contains(path) {
                return Err(VfsError::Io {
                    path: path.to_path_buf(),
                    message: "permission denied".to_owned(),
                });
            }

            let mut entries = BTreeSet::new();
            for file in self.files.keys() {
                let Ok(rest) = file.strip_prefix(path) else {
                    continue;
                };
                let mut components = rest.components();
                let Some(first) = components.next() else {
                    continue;
                };
                let child = path.join(first);
                // The real implementations hide these, so the fake has to as well or a test
                // proves something the game does not do.
                let name = first.as_os_str().to_string_lossy().into_owned();
                if name.starts_with('.') || name.to_ascii_lowercase().ends_with(".meta") {
                    continue;
                }
                entries.insert(DirEntry {
                    path: child,
                    is_dir: components.next().is_some(),
                });
            }

            if entries.is_empty() {
                return Err(VfsError::NotFound(path.to_path_buf()));
            }
            // `BTreeSet` already sorted them, which is the ordering guarantee the trait makes.
            Ok(entries.into_iter().collect())
        })
    }
}
