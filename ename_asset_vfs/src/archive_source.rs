//! Archives under Bevy's `Default` asset source.
//!
//! [`ArchiveSourcePlugin`] registers the `Default` source as the platform reader wrapped in an
//! archive reader, so a plain path, an `alias://` path and a scan all read archive members as if
//! they were loose files. Its writer refuses anything inside a mount, and [`check_writable`] does
//! the same for writes that bypass Bevy.
//!
//! Compiled with the `bevy` feature alone, so the guard is present whichever crate turned an
//! archive format on.

use crate::{Mounts, Origin, mount::Answer};
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
    ecs::world::World,
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

/// Refuses a filesystem write into an archive mount, or onto a mounted archive file. `path` is a
/// filesystem path, absolute or relative to the current directory; the asset root is resolved
/// with symlinks first. Does nothing when no [`ArchiveMounts`] is present.
pub fn check_writable(world: &World, path: &Path) -> io::Result<()> {
    let Some(mounts) = world.get_resource::<ArchiveMounts>() else {
        return Ok(());
    };
    match mounts.0.origin_of_file(path) {
        Origin::Loose => Ok(()),
        _ => Err(read_only(path)),
    }
}

fn read_only(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::ReadOnlyFilesystem,
        format!(
            "{} is inside an archive or is a mounted archive, which is read-only",
            path.display()
        ),
    )
}

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
            .with_watcher(archive_watcher(self.asset_root.clone(), mounts.clone()))
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
    mounts: Arc<Mounts>,
) -> impl FnMut(Sender<AssetSourceEvent>) -> Option<Box<dyn AssetWatcher>> + Send + Sync + 'static {
    let mut make_inner =
        AssetSource::get_default_watcher(asset_root, std::time::Duration::from_millis(300));
    move |sink| {
        let (sender, events) = async_channel::unbounded();
        let inner = make_inner(sender)?;
        let task = IoTaskPool::get().spawn(forward_loose_events(events, sink, mounts.clone()));
        Some(Box::new(ArchiveWatcher {
            _inner: inner,
            _task: task,
        }))
    }
}

/// Keeps receiving after `sink` closes: Bevy's file watcher unwraps its send, so a dropped
/// receiver would panic its thread.
async fn forward_loose_events(
    events: Receiver<AssetSourceEvent>,
    sink: Sender<AssetSourceEvent>,
    mounts: Arc<Mounts>,
) {
    while let Ok(event) = events.recv().await {
        if let Some(archive) = archive_in(&mounts, &event) {
            info!(
                "{} changed; restart to apply changes to an archive",
                archive.display()
            );
            continue;
        }
        let _ = sink.send(event).await;
    }
}

/// The mounted archive file an event touches, if any. Answered from what is already known, so a
/// directory merely named `x.zip` is never mistaken for one.
fn archive_in<'e>(mounts: &Mounts, event: &'e AssetSourceEvent) -> Option<&'e Path> {
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
        .find(|path| mounts.is_known_archive(path))
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
///
/// Anything already known to be loose, or answerable from a cached index, is answered inline. Only
/// listing a directory for the first time, indexing an archive and reading member bytes hop to a
/// blocking thread.
pub(crate) struct ArchiveAssetReader {
    inner: Box<dyn ErasedAssetReader>,
    mounts: Arc<Mounts>,
}

impl ArchiveAssetReader {
    /// `answer` from memory, else on a blocking thread. `None` means loose: the caller delegates
    /// to the platform reader.
    async fn mounted<T: Send + 'static>(
        &self,
        path: &Path,
        answer: fn(&Mounts, &Path, bool) -> Answer<T>,
    ) -> Option<Result<T, AssetReaderError>> {
        let result = match answer(&self.mounts, path, false) {
            Answer::Loose => return None,
            Answer::Ready(result) => result,
            Answer::NeedsIo => {
                let mounts = self.mounts.clone();
                let owned = path.to_path_buf();
                match blocking::unblock(move || answer(&mounts, &owned, true)).await {
                    Answer::Loose => return None,
                    Answer::Ready(result) => result,
                    Answer::NeedsIo => unreachable!("filesystem work was allowed"),
                }
            }
        };
        Some(result.map_err(|err| reader_error(path, err)))
    }
}

impl AssetReader for ArchiveAssetReader {
    async fn read<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        match self.mounted(path, Mounts::read_file_answer).await {
            Some(bytes) => Ok(Box::new(VecReader::new(bytes?)) as Box<dyn Reader + 'a>),
            None => self.inner.read(path).await,
        }
    }

    async fn read_meta<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        let meta = meta_path(path);
        match self.mounted(&meta, Mounts::read_file_answer).await {
            Some(bytes) => Ok(Box::new(VecReader::new(bytes?)) as Box<dyn Reader + 'a>),
            None => self.inner.read_meta(path).await,
        }
    }

    fn read_directory<'a>(
        &'a self,
        path: &'a Path,
    ) -> impl ConditionalSendFuture<Output = Result<Box<PathStream>, AssetReaderError>> {
        async move {
            let paths: Vec<PathBuf> = match self.mounted(path, Mounts::read_dir_answer).await {
                Some(entries) => entries?.into_iter().map(|entry| entry.path).collect(),
                None => {
                    let loose: Vec<PathBuf> =
                        self.inner.read_directory(path).await?.collect().await;
                    let overlaid = match self.mounts.overlay_listing(path, &loose, false) {
                        Some(overlaid) => Some(overlaid),
                        None => {
                            let mounts = self.mounts.clone();
                            let dir = path.to_path_buf();
                            let entries = loose.clone();
                            blocking::unblock(move || mounts.overlay_listing(&dir, &entries, true))
                                .await
                        }
                    };
                    match overlaid {
                        Some(overlaid) => overlaid.into_iter().map(|(path, _)| path).collect(),
                        None => loose,
                    }
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
            match self.mounted(path, Mounts::is_dir_answer).await {
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
pub(crate) struct GuardedWriter {
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
                    return Err(AssetWriterError::Io(read_only(&path)));
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

#[cfg(all(test, feature = "zip"))]
mod tests {
    use super::{AssetSourceEvent as E, archive_in};
    use crate::Mounts;
    use std::path::{Path, PathBuf};

    #[test]
    fn only_events_for_mounted_archive_files_are_dropped() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("mods/dir.zip")).unwrap();
        std::fs::copy(
            "tests/fixtures/archives/basic.zip",
            root.path().join("mods/core.zip"),
        )
        .unwrap();
        let mounts = Mounts::new(root.path());
        // Nothing has looked at `mods` yet, so nothing there is mounted.
        assert_eq!(
            archive_in(&mounts, &E::ModifiedAsset(PathBuf::from("mods/core.zip"))),
            None
        );
        mounts.origin(Path::new("mods/core"));
        let p = PathBuf::from;
        assert_eq!(
            archive_in(&mounts, &E::ModifiedAsset(p("mods/core.zip"))),
            Some(Path::new("mods/core.zip"))
        );
        let rename = E::RenamedAsset {
            old: p("mods/.core.zip.tmp"),
            new: p("mods/core.zip"),
        };
        assert_eq!(
            archive_in(&mounts, &rename),
            Some(Path::new("mods/core.zip"))
        );
        for event in [
            E::ModifiedAsset(p("mods/core/a.txt")),
            E::AddedFolder(p("mods/dir.zip")),
            E::ModifiedAsset(p("mods/dir.zip/a.txt")),
        ] {
            assert_eq!(archive_in(&mounts, &event), None, "{event:?}");
        }
    }
}
