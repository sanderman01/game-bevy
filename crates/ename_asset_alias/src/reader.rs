//! The `alias://` asset source.
//!
//! `asset_server.load("alias://core::airship#Scene0")` returns a handle on frame 0. The reader
//! awaits the index, resolves the alias to a real path, and delegates to the platform default
//! reader. Waiting happens inside `bevy_asset`, which is where waiting on io already belongs.
//!
//! A handle is keyed on the alias rather than on the resolved path, so which package won an
//! override is invisible to game code and to anything serialized.
//!
//! **Whatever fills the cells this reader awaits must never read through this source.** It has to
//! read the default source directly. Going through `alias://` would await a cell only that code
//! can fill, and hang.

use crate::ContentIndex;
use async_lock::OnceCell;
use bevy::{
    app::{App, Plugin, Startup},
    asset::{
        AssetApp, AssetServer,
        io::{
            AssetReader, AssetReaderError, AssetSource, AssetSourceBuilder, AssetSourceId,
            ErasedAssetReader, PathStream, Reader, VecReader,
        },
    },
    ecs::{resource::Resource, system::Res},
    tasks::ConditionalSendFuture,
};
use std::{path::Path, sync::Arc};

/// The asset source name. Paths look like `alias://core::airship#Scene0`.
pub const ALIAS_SOURCE: &str = "alias";

/// Relative path to the asset root. Mirrors `AssetPlugin::file_path`'s default.
/// `BEVY_ASSET_ROOT` is applied inside the reader, not here.
const DEFAULT_ASSET_ROOT: &str = "assets";

/// Mirrors `AssetPlugin::DEFAULT_PROCESSED_FILE_PATH`.
const DEFAULT_PROCESSED_ASSET_ROOT: &str = "imported_assets/Default";

/// The shared cell the `alias://` reader awaits. Empty until somebody fills it.
///
/// The field is public because filling it is the whole contract: `ename_asset_content` sets it
/// from its scan task, and a test sets it directly.
#[derive(Resource, Clone, Default)]
pub struct ContentIndexCell(pub Arc<OnceCell<ContentIndex>>);

/// The [`AssetServer`] the reader consults to synthesize a missing `.meta`. Filled on `Startup`,
/// because `AliasSourcePlugin` builds before `AssetPlugin` and the server does not exist yet.
///
/// This is a reference cycle: the server owns the source, the source owns the reader, the reader
/// owns the server. It costs one leaked `AssetServer` at process exit and buys the reader a
/// loader lookup it has no other way to reach. See [`AliasReader::synthesized_meta`].
#[derive(Resource, Clone, Default)]
struct AssetServerCell(Arc<OnceCell<AssetServer>>);

/// Resolves an alias to a path under the asset root, then delegates to `inner`.
pub(crate) struct AliasReader {
    index: Arc<OnceCell<ContentIndex>>,
    server: Arc<OnceCell<AssetServer>>,
    inner: Box<dyn ErasedAssetReader>,
}

impl AliasReader {
    pub(crate) fn new(
        index: Arc<OnceCell<ContentIndex>>,
        server: Arc<OnceCell<AssetServer>>,
        inner: Box<dyn ErasedAssetReader>,
    ) -> Self {
        Self {
            index,
            server,
            inner,
        }
    }

    /// Awaits the index, then maps `path` onto the real asset path.
    ///
    /// The result borrows from the index rather than being owned, because
    /// `ErasedAssetReader::read` ties the path's lifetime to the reader it hands back. A local
    /// `PathBuf` here would make every delegated read borrow something already dropped.
    async fn resolve<'a>(&'a self, path: &'a Path) -> Result<&'a Path, AssetReaderError> {
        let not_found = || AssetReaderError::NotFound(path.to_path_buf());
        let index = self.index.wait().await;
        let alias = path.to_str().ok_or_else(not_found)?;
        index.resolve(alias).ok_or_else(not_found)
    }

    /// The default `.meta` of whichever loader claims `path`'s extension, serialized as if it had
    /// been read off disk.
    ///
    /// An alias carries no file extension, and `AssetLoaders::find` skips the by-asset-type lookup
    /// whenever the `AssetPath` has a label, so `alias://core::airship#Scene0` would otherwise
    /// resolve no loader at all and fail with `MissingAssetLoader`. Answering `read_meta` with the
    /// resolved file's default meta puts the loader name back in the one place `bevy_asset` still
    /// looks, and keeps the extension out of the address.
    ///
    /// Returns `None` when nothing claims the extension, so the caller reports the meta as absent
    /// and Bevy falls back to its own loader search.
    async fn synthesized_meta(&self, path: &Path) -> Option<VecReader> {
        let server = self.server.wait().await;
        let file_name = path.file_name()?.to_str()?;
        let full_extension = &file_name[file_name.find('.')? + 1..];
        // Same widening Bevy applies to a path's own extension: `a.b.c` also tries `b.c` and `c`.
        let secondary = full_extension
            .char_indices()
            .filter_map(|(i, c)| (c == '.').then_some(&full_extension[i + 1..]));
        for extension in std::iter::once(full_extension).chain(secondary) {
            if let Ok(loader) = server.get_asset_loader_with_extension(extension).await {
                return Some(VecReader::new(loader.default_meta().serialize()));
            }
        }
        None
    }
}

impl AssetReader for AliasReader {
    async fn read<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        let real = self.resolve(path).await?;
        self.inner.read(real).await
    }

    /// Delegated to the real path so Bevy's own `.meta` is still found. Losing that would lose
    /// the loader settings. Where the asset has no `.meta`, the loader for its extension supplies
    /// one, which is what makes a labelled alias loadable at all.
    async fn read_meta<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        let real = self.resolve(path).await?;
        match self.inner.read_meta(real).await {
            Ok(reader) => Ok(reader),
            Err(AssetReaderError::NotFound(missing)) => match self.synthesized_meta(real).await {
                Some(reader) => Ok(Box::new(reader) as Box<dyn Reader + 'a>),
                None => Err(AssetReaderError::NotFound(missing)),
            },
            Err(err) => Err(err),
        }
    }

    fn read_directory<'a>(
        &'a self,
        path: &'a Path,
    ) -> impl ConditionalSendFuture<Output = Result<Box<PathStream>, AssetReaderError>> {
        // Listing by alias prefix lands with the folder rules in phase 2.
        async move { Err(AssetReaderError::NotFound(path.to_path_buf())) }
    }

    fn is_directory<'a>(
        &'a self,
        path: &'a Path,
    ) -> impl ConditionalSendFuture<Output = Result<bool, AssetReaderError>> {
        let _ = path;
        async move { Ok(false) }
    }
}

/// Registers the `alias://` asset source and the [`ContentIndexCell`] it reads through.
///
/// This plugin does not fill the cell. `ename_asset_content` does that from a scan; a test does it
/// directly.
pub struct AliasSourcePlugin {
    asset_root: String,
    processed_asset_root: String,
}

impl Default for AliasSourcePlugin {
    fn default() -> Self {
        Self {
            asset_root: DEFAULT_ASSET_ROOT.to_owned(),
            processed_asset_root: DEFAULT_PROCESSED_ASSET_ROOT.to_owned(),
        }
    }
}

impl AliasSourcePlugin {
    /// Sets the asset root the alias source reads through. Must match `AssetPlugin::file_path`.
    pub fn with_asset_root(mut self, path: impl Into<String>) -> Self {
        self.asset_root = path.into();
        self
    }

    /// Sets the processed asset root. Must match `AssetPlugin::processed_file_path`.
    pub fn with_processed_asset_root(mut self, path: impl Into<String>) -> Self {
        self.processed_asset_root = path.into();
        self
    }
}

impl Plugin for AliasSourcePlugin {
    fn build(&self, app: &mut App) {
        // `register_asset_source` only inserts into the `AssetSourceBuilders` resource.
        // `AssetPlugin::build` is what turns that resource into live `AssetSources`, and it does
        // so once. If `AssetPlugin` has already built, Bevy logs an error and the source is never
        // constructed (`bevy_asset/src/lib.rs:614`); every `alias://` load then fails with a
        // missing-source error that says nothing about ordering.
        assert!(
            app.world().get_resource::<AssetServer>().is_none(),
            "AliasSourcePlugin must be added before AssetPlugin. `register_asset_source` only \
             fills AssetSourceBuilders, and AssetPlugin builds that resource into live sources \
             exactly once. Registering afterwards leaves `alias://` dead."
        );

        let cell = ContentIndexCell::default();
        let server_cell = AssetServerCell::default();

        // Both readers get registered: `with_reader` supplies the unprocessed one and
        // `with_processed_reader` the processed one, and Bevy picks by mode. Registering only the
        // first would quietly serve unprocessed assets the moment `asset_processor` is turned on.
        let reader_cell = cell.0.clone();
        let reader_server = server_cell.0.clone();
        let mut make_reader = AssetSource::get_default_reader(self.asset_root.clone());
        let processed_cell = cell.0.clone();
        let processed_server = server_cell.0.clone();
        let mut make_processed_reader =
            AssetSource::get_default_reader(self.processed_asset_root.clone());

        app.register_asset_source(
            AssetSourceId::from(ALIAS_SOURCE),
            AssetSourceBuilder::new(move || {
                Box::new(AliasReader::new(
                    reader_cell.clone(),
                    reader_server.clone(),
                    make_reader(),
                ))
            })
            .with_processed_reader(move || {
                Box::new(AliasReader::new(
                    processed_cell.clone(),
                    processed_server.clone(),
                    make_processed_reader(),
                ))
            }),
        )
        .insert_resource(cell)
        .insert_resource(server_cell)
        .add_systems(Startup, capture_asset_server);
    }
}

/// Hands the reader the [`AssetServer`], which did not exist when this plugin built.
///
/// `Startup` rather than `Plugin::finish`, because `App::update` does not run `finish` and a test
/// that drives frames by hand would wait on the cell forever.
fn capture_asset_server(cell: Res<AssetServerCell>, server: Res<AssetServer>) {
    let _ = cell.0.set_blocking(server.clone());
}
