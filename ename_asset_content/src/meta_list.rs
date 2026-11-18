//! The browser's `Default` source: Bevy's reader, asking for a `.meta` only where the build saw one.
//!
//! Bevy asks for every asset's `.meta` before loading it. Over HTTP a missing one is a 404 and a
//! round trip per asset, and almost none exist. `ename_web_build` lists the ones that do in
//! [`META_LIST_FILE`]. Without that file, every `.meta` is asked for as before.

use async_lock::OnceCell;
use bevy::{
    asset::io::{AssetReader, AssetReaderError, ErasedAssetReader, PathStream, Reader},
    tasks::ConditionalSendFuture,
};
use ename_asset_vfs::META_LIST_FILE;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

pub(crate) struct MetaListReader {
    inner: Box<dyn ErasedAssetReader>,
    /// `None` when the list could not be read: then every `.meta` may exist.
    known: OnceCell<Option<HashSet<PathBuf>>>,
}

impl MetaListReader {
    pub(crate) fn new(inner: Box<dyn ErasedAssetReader>) -> Self {
        Self {
            inner,
            known: OnceCell::new(),
        }
    }

    async fn may_exist(&self, meta: &Path) -> bool {
        let known = self
            .known
            .get_or_init(|| async {
                let mut reader = self.inner.read(Path::new(META_LIST_FILE)).await.ok()?;
                let mut bytes = Vec::new();
                reader.read_to_end(&mut bytes).await.ok()?;
                Some(
                    String::from_utf8_lossy(&bytes)
                        .lines()
                        .filter(|line| !line.is_empty())
                        .map(PathBuf::from)
                        .collect(),
                )
            })
            .await;
        known.as_ref().is_none_or(|known| known.contains(meta))
    }
}

impl AssetReader for MetaListReader {
    async fn read<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        self.inner.read(path).await
    }

    async fn read_meta<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        let mut meta = path.as_os_str().to_owned();
        meta.push(".meta");
        let meta = PathBuf::from(meta);
        if self.may_exist(&meta).await {
            self.inner.read_meta(path).await
        } else {
            Err(AssetReaderError::NotFound(meta))
        }
    }

    fn read_directory<'a>(
        &'a self,
        path: &'a Path,
    ) -> impl ConditionalSendFuture<Output = Result<Box<PathStream>, AssetReaderError>> {
        self.inner.read_directory(path)
    }

    fn is_directory<'a>(
        &'a self,
        path: &'a Path,
    ) -> impl ConditionalSendFuture<Output = Result<bool, AssetReaderError>> {
        self.inner.is_directory(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::io::memory::{Dir, MemoryAssetReader};
    use bevy::tasks::block_on;

    fn reader(list: Option<&str>) -> MetaListReader {
        let root = Dir::new(PathBuf::new());
        root.insert_asset_text(Path::new("a.png"), "a");
        root.insert_meta_text(Path::new("a.png"), "meta a");
        // Present, but not in the list: proves the list is what answers.
        root.insert_asset_text(Path::new("b.png"), "b");
        root.insert_meta_text(Path::new("b.png"), "meta b");
        if let Some(list) = list {
            root.insert_asset_text(Path::new(META_LIST_FILE), list);
        }
        MetaListReader::new(Box::new(MemoryAssetReader { root }))
    }

    fn has_meta(reader: &MetaListReader, path: &str) -> bool {
        block_on(AssetReader::read_meta(reader, Path::new(path))).is_ok()
    }

    #[test]
    fn only_listed_metas_are_asked_for() {
        let reader = reader(Some("a.png.meta\n"));
        assert!(has_meta(&reader, "a.png"));
        assert!(!has_meta(&reader, "b.png"));
    }

    #[test]
    fn without_a_list_every_meta_is_asked_for() {
        let reader = reader(None);
        assert!(has_meta(&reader, "a.png"));
        assert!(has_meta(&reader, "b.png"));
    }
}
