//! `ename_asset_content` -- the glue between `ename_asset_alias` and `ename_asset_package`.
//!
//! The two crates below this one are deliberately unaware of each other: one knows what an alias
//! is, the other knows what a package is, and neither needs the other to be useful. This crate is
//! where they meet. It is also where the plugin lives, because deciding what goes in an `App` is
//! composition, not asset logic. See `docs/design/crate-layout.md`.

mod index;
mod report;

pub use crate::index::build_index;
pub use crate::report::{ContentReport, PackageSummary};

use bevy::{
    app::{App, Plugin, Startup},
    asset::io::AssetSource,
    ecs::{resource::Resource, system::Res},
    log::info,
    tasks::IoTaskPool,
};
use ename_asset_alias::{AliasSourcePlugin, ContentIndexCell};
use ename_asset_package::{AssetReaderVfs, scan_packages};

/// Relative path to the asset root. Mirrors `AssetPlugin::file_path`'s default.
const DEFAULT_ASSET_ROOT: &str = "assets";

/// No search paths by default. Which directories a game ships is game policy: the target passes
/// them in.
const DEFAULT_SEARCH_PATHS: &[&str] = &[];

/// Adds the `alias://` asset source and the startup scan that fills its index.
///
/// **Must be added before `AssetPlugin`.** `ename_engine` does that with
/// `add_before::<AssetPlugin>`, and `AliasSourcePlugin` asserts on it.
pub struct AssetContentPlugin {
    search_paths: Vec<String>,
    asset_root: String,
}

impl Default for AssetContentPlugin {
    fn default() -> Self {
        Self {
            search_paths: DEFAULT_SEARCH_PATHS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            asset_root: DEFAULT_ASSET_ROOT.to_owned(),
        }
    }
}

impl AssetContentPlugin {
    /// Sets the directories scanned for packages, relative to the asset root.
    pub fn with_search_paths<I, S>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.search_paths = paths.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the asset root. Must match `AssetPlugin::file_path`.
    pub fn with_asset_root(mut self, path: impl Into<String>) -> Self {
        self.asset_root = path.into();
        self
    }
}

/// What [`start_content_scan`] needs, kept out of the plugin so the system can read it.
#[derive(Resource, Clone)]
struct ContentScanConfig {
    search_paths: Vec<String>,
    asset_root: String,
}

impl Plugin for AssetContentPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(AliasSourcePlugin::default().with_asset_root(self.asset_root.clone()))
            .insert_resource(ContentScanConfig {
                search_paths: self.search_paths.clone(),
                asset_root: self.asset_root.clone(),
            })
            .add_systems(Startup, start_content_scan);
    }
}

/// Spawns the package scan and detaches it. The task fills [`ContentIndexCell`], which wakes every
/// `alias://` read waiting on it, so nothing here needs polling or a state machine.
///
/// The scan builds its own reader over the default source. It must never read through `alias://`:
/// that would await an index only this task can fill, and hang.
fn start_content_scan(cell: Res<ContentIndexCell>, config: Res<ContentScanConfig>) {
    let cell = cell.0.clone();
    let config = config.clone();
    IoTaskPool::get()
        .spawn(async move {
            info!("Scanning for packages in {:?}", config.search_paths);
            let mut make_reader = AssetSource::get_default_reader(config.asset_root);
            let vfs = AssetReaderVfs::new(make_reader());
            let scan = scan_packages(&vfs, &config.search_paths).await;
            let (index, _report) = build_index(&scan);
            let _ = cell.set(index).await;
        })
        .detach();
}
