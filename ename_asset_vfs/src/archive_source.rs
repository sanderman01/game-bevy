//! Archives under Bevy's `Default` asset source.
//!
//! [`ArchiveSourcePlugin`] registers the `Default` source as the platform reader wrapped in an
//! [`ArchiveAssetReader`], so a plain path, an `alias://` path and a scan all read archive members
//! as if they were loose files. Its writer refuses anything inside a mount.

use crate::{Mounts, mount::mount_path_of};
use async_channel::{Receiver, Sender};
use bevy::{
    app::{App, Plugin, Startup},
    asset::{
        AssetApp, AssetServer, AssetServerMode,
        io::{
            AssetReader, AssetReaderError, AssetSource, AssetSourceBuilder, AssetSourceEvent,
            AssetSourceId, AssetWatcher, AssetWriter, AssetWriterError, ErasedAssetReader,
            ErasedAssetWriter, PathStream, Reader, VecReader, Writer, file::FileAssetReader,
        },
    },
    ecs::{resource::Resource, system::Res},
    log::{error, info},
    tasks::{ConditionalSendFuture, IoTaskPool, Task, futures_lite::StreamExt},
};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

/// The archive mounts behind the `Default` source, for anything that writes to the asset tree
/// outside Bevy's writer and for reporting [`Mounts::problems`].
#[derive(Resource, Clone)]
pub struct ArchiveMounts(pub Arc<Mounts>);

/// Mounts every `.zip`/`.tar` under the asset root as a directory, by registering Bevy's
/// `Default` asset source. Inserts [`ArchiveMounts`].
///
/// **Must be added before `AssetPlugin`**, which only fills the `Default` source when nothing has
/// yet. Unprocessed mode only: the processed reader and writer are not registered.
pub struct ArchiveSourcePlugin {
    asset_root: String,
}

impl Default for ArchiveSourcePlugin {
    fn default() -> Self {
        Self {
            asset_root: "assets".to_owned(),
        }
    }
}

impl ArchiveSourcePlugin {
    /// Must match `AssetPlugin::file_path`.
    pub fn with_asset_root(mut self, path: impl Into<String>) -> Self {
        self.asset_root = path.into();
        self
    }
}

impl Plugin for ArchiveSourcePlugin {
    fn build(&self, app: &mut App) {
        assert!(
            app.world().get_resource::<AssetServer>().is_none(),
            "ArchiveSourcePlugin must be added before AssetPlugin, which builds the Default asset \
             source exactly once."
        );
        // Exactly where `FileAssetReader` looks, `BEVY_ASSET_ROOT` included.
        let mounts = Arc::new(Mounts::new(
            FileAssetReader::get_base_path().join(&self.asset_root),
        ));
        let reader_mounts = mounts.clone();
        let writer_mounts = mounts.clone();
        let mut make_reader = AssetSource::get_default_reader(self.asset_root.clone());
        let mut make_writer = AssetSource::get_default_writer(self.asset_root.clone());
        app.register_asset_source(
            AssetSourceId::Default,
            AssetSourceBuilder::new(move || {
                Box::new(ArchiveAssetReader {
                    inner: make_reader(),
                    mounts: reader_mounts.clone(),
                })
            })
            .with_writer(move |create_root| {
                let inner = make_writer(create_root)?;
                Some(Box::new(GuardedWriter {
                    inner,
                    mounts: writer_mounts.clone(),
                }))
            })
            .with_watcher(archive_watcher(self.asset_root.clone()))
            .with_watch_warning(AssetSource::get_default_watch_warning()),
        )
        .insert_resource(ArchiveMounts(mounts))
        .add_systems(Startup, refuse_processed_mode);
    }
}

/// The platform watcher, with events for archive files dropped: archives are indexed once, so a
/// changed one needs a restart. Dropping the task cancels it.
struct ArchiveWatcher {
    _inner: Box<dyn AssetWatcher>,
    _task: Task<()>,
}

impl AssetWatcher for ArchiveWatcher {}

fn archive_watcher(
    asset_root: String,
) -> impl FnMut(Sender<AssetSourceEvent>) -> Option<Box<dyn AssetWatcher>> + Send + Sync + 'static {
    let mut make_inner =
        AssetSource::get_default_watcher(asset_root, std::time::Duration::from_millis(300));
    move |sink| {
        let (sender, events) = async_channel::unbounded();
        let inner = make_inner(sender)?;
        let task = IoTaskPool::get().spawn(forward_loose_events(events, sink));
        Some(Box::new(ArchiveWatcher {
            _inner: inner,
            _task: task,
        }))
    }
}

/// Keeps receiving after `sink` closes: Bevy's file watcher unwraps its send, so a dropped
/// receiver would panic its thread.
async fn forward_loose_events(events: Receiver<AssetSourceEvent>, sink: Sender<AssetSourceEvent>) {
    while let Ok(event) = events.recv().await {
        if let Some(archive) = archive_in(&event) {
            info!(
                "{} changed; restart to apply changes to an archive",
                archive.display()
            );
            continue;
        }
        let _ = sink.send(event).await;
    }
}

/// The archive file an event touches, if any.
fn archive_in(event: &AssetSourceEvent) -> Option<&Path> {
    use AssetSourceEvent as E;
    let paths: [Option<&PathBuf>; 2] = match event {
        E::AddedAsset(path)
        | E::ModifiedAsset(path)
        | E::RemovedAsset(path)
        | E::AddedMeta(path)
        | E::ModifiedMeta(path)
        | E::RemovedMeta(path)
        | E::AddedFolder(path)
        | E::RemovedFolder(path)
        | E::RemovedUnknown { path, .. } => [Some(path), None],
        E::RenamedAsset { old, new }
        | E::RenamedMeta { old, new }
        | E::RenamedFolder { old, new } => [Some(old), Some(new)],
    };
    paths
        .into_iter()
        .flatten()
        .find(|path| mount_path_of(path).is_some())
        .map(PathBuf::as_path)
}

fn refuse_processed_mode(server: Res<AssetServer>) {
    if server.mode() == AssetServerMode::Processed {
        error!(
            "ArchiveSourcePlugin supports AssetMode::Unprocessed only; processed reads of the \
             Default source will fail"
        );
    }
}

/// The platform reader, with archive members served from [`Mounts`].
pub struct ArchiveAssetReader {
    inner: Box<dyn ErasedAssetReader>,
    mounts: Arc<Mounts>,
}

impl ArchiveAssetReader {
    /// Runs `query` against the mounts on a blocking thread, unless `path` is already known to be
    /// loose. `None` means loose: the caller delegates to the platform reader.
    async fn mounted<T: Send + 'static>(
        &self,
        path: &Path,
        include_last: bool,
        query: impl FnOnce(&Mounts, &Path) -> Option<io::Result<T>> + Send + 'static,
    ) -> Option<Result<T, AssetReaderError>> {
        if self.mounts.is_known_loose(path, include_last) {
            return None;
        }
        let mounts = self.mounts.clone();
        let owned = path.to_path_buf();
        let result = blocking::unblock(move || query(&mounts, &owned)).await?;
        Some(result.map_err(|err| reader_error(path, err)))
    }
}

impl AssetReader for ArchiveAssetReader {
    async fn read<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        match self
            .mounted(path, false, |mounts, path| mounts.read_file(path))
            .await
        {
            Some(bytes) => Ok(Box::new(VecReader::new(bytes?)) as Box<dyn Reader + 'a>),
            None => self.inner.read(path).await,
        }
    }

    async fn read_meta<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        let meta = meta_path(path);
        match self
            .mounted(&meta, false, |mounts, meta| mounts.read_file(meta))
            .await
        {
            Some(bytes) => Ok(Box::new(VecReader::new(bytes?)) as Box<dyn Reader + 'a>),
            None => self.inner.read_meta(path).await,
        }
    }

    fn read_directory<'a>(
        &'a self,
        path: &'a Path,
    ) -> impl ConditionalSendFuture<Output = Result<Box<PathStream>, AssetReaderError>> {
        async move {
            let paths: Vec<PathBuf> = match self
                .mounted(path, true, |mounts, path| mounts.read_dir(path))
                .await
            {
                Some(entries) => entries?.into_iter().map(|entry| entry.path).collect(),
                None => {
                    let loose: Vec<PathBuf> =
                        self.inner.read_directory(path).await?.collect().await;
                    let mounts = self.mounts.clone();
                    blocking::unblock(move || {
                        mounts
                            .overlay_listing(loose)
                            .into_iter()
                            .map(|(path, _)| path)
                            .collect()
                    })
                    .await
                }
            };
            let stream: Box<PathStream> = Box::new(bevy::tasks::futures_lite::stream::iter(paths));
            Ok(stream)
        }
    }

    fn is_directory<'a>(
        &'a self,
        path: &'a Path,
    ) -> impl ConditionalSendFuture<Output = Result<bool, AssetReaderError>> {
        async move {
            match self
                .mounted(path, true, |mounts, path| mounts.is_dir(path))
                .await
            {
                Some(is_dir) => is_dir,
                None => self.inner.is_directory(path).await,
            }
        }
    }
}

fn meta_path(path: &Path) -> PathBuf {
    let mut meta = path.as_os_str().to_owned();
    meta.push(".meta");
    meta.into()
}

fn reader_error(path: &Path, err: io::Error) -> AssetReaderError {
    match err.kind() {
        io::ErrorKind::NotFound => AssetReaderError::NotFound(path.to_path_buf()),
        _ => AssetReaderError::Io(Arc::new(err)),
    }
}

/// The platform writer, refusing every path inside a mount.
pub struct GuardedWriter {
    inner: Box<dyn ErasedAssetWriter>,
    mounts: Arc<Mounts>,
}

impl GuardedWriter {
    async fn check(&self, paths: &[&Path]) -> Result<(), AssetWriterError> {
        let mounts = self.mounts.clone();
        let paths: Vec<PathBuf> = paths.iter().map(|path| path.to_path_buf()).collect();
        blocking::unblock(move || {
            for path in paths {
                if !mounts.origin(&path).is_loose() {
                    return Err(AssetWriterError::Io(io::Error::new(
                        io::ErrorKind::ReadOnlyFilesystem,
                        format!(
                            "{} is inside an archive, which is read-only",
                            path.display()
                        ),
                    )));
                }
            }
            Ok(())
        })
        .await
    }
}

impl AssetWriter for GuardedWriter {
    async fn write<'a>(&'a self, path: &'a Path) -> Result<Box<Writer>, AssetWriterError> {
        self.check(&[path]).await?;
        self.inner.write(path).await
    }

    async fn write_meta<'a>(&'a self, path: &'a Path) -> Result<Box<Writer>, AssetWriterError> {
        self.check(&[path]).await?;
        self.inner.write_meta(path).await
    }

    async fn remove<'a>(&'a self, path: &'a Path) -> Result<(), AssetWriterError> {
        self.check(&[path]).await?;
        self.inner.remove(path).await
    }

    async fn remove_meta<'a>(&'a self, path: &'a Path) -> Result<(), AssetWriterError> {
        self.check(&[path]).await?;
        self.inner.remove_meta(path).await
    }

    async fn rename<'a>(
        &'a self,
        old_path: &'a Path,
        new_path: &'a Path,
    ) -> Result<(), AssetWriterError> {
        self.check(&[old_path, new_path]).await?;
        self.inner.rename(old_path, new_path).await
    }

    async fn rename_meta<'a>(
        &'a self,
        old_path: &'a Path,
        new_path: &'a Path,
    ) -> Result<(), AssetWriterError> {
        self.check(&[old_path, new_path]).await?;
        self.inner.rename_meta(old_path, new_path).await
    }

    async fn create_directory<'a>(&'a self, path: &'a Path) -> Result<(), AssetWriterError> {
        self.check(&[path]).await?;
        self.inner.create_directory(path).await
    }

    async fn remove_directory<'a>(&'a self, path: &'a Path) -> Result<(), AssetWriterError> {
        self.check(&[path]).await?;
        self.inner.remove_directory(path).await
    }

    async fn remove_empty_directory<'a>(&'a self, path: &'a Path) -> Result<(), AssetWriterError> {
        self.check(&[path]).await?;
        self.inner.remove_empty_directory(path).await
    }

    async fn remove_assets_in_directory<'a>(
        &'a self,
        path: &'a Path,
    ) -> Result<(), AssetWriterError> {
        self.check(&[path]).await?;
        self.inner.remove_assets_in_directory(path).await
    }

    async fn write_bytes<'a>(
        &'a self,
        path: &'a Path,
        bytes: &'a [u8],
    ) -> Result<(), AssetWriterError> {
        self.check(&[path]).await?;
        self.inner.write_bytes(path, bytes).await
    }

    async fn write_meta_bytes<'a>(
        &'a self,
        path: &'a Path,
        bytes: &'a [u8],
    ) -> Result<(), AssetWriterError> {
        self.check(&[path]).await?;
        self.inner.write_meta_bytes(path, bytes).await
    }
}

#[cfg(test)]
mod tests {
    use super::{AssetSourceEvent as E, archive_in};
    use std::path::{Path, PathBuf};

    #[test]
    fn events_for_archive_files_are_recognised() {
        let p = PathBuf::from;
        assert_eq!(
            archive_in(&E::ModifiedAsset(p("mods/core.zip"))),
            Some(Path::new("mods/core.zip"))
        );
        let rename = E::RenamedAsset {
            old: p("mods/.core.tar.tmp"),
            new: p("mods/core.tar"),
        };
        assert_eq!(archive_in(&rename), Some(Path::new("mods/core.tar")));
        assert_eq!(archive_in(&E::ModifiedAsset(p("mods/core/a.txt"))), None);
        assert_eq!(archive_in(&E::AddedFolder(p("mods/core"))), None);
    }
}
