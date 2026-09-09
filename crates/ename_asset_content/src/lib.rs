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
pub use ename_asset_alias::{Problem, ProblemKind};

use async_lock::OnceCell;
use bevy::{
    app::{App, Plugin, Startup, Update},
    asset::io::AssetSource,
    ecs::{
        resource::Resource,
        system::{Commands, Res},
    },
    log::info,
    tasks::IoTaskPool,
};
use ename_asset_alias::{AliasSourcePlugin, AssetReaderVfs, ContentIndexCell};
use ename_asset_package::scan_packages;
use std::sync::Arc;

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

/// Where [`start_content_scan`] leaves the report for [`mirror_content_scan`] to pick up.
///
/// Private: the report reaches the rest of the world as `Res<ContentReport>`, and a second way to
/// read it would be a second thing to keep in step.
#[derive(Resource, Clone, Default)]
struct ContentReportCell(Arc<OnceCell<ContentReport>>);

impl Plugin for AssetContentPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(AliasSourcePlugin::default().with_asset_root(self.asset_root.clone()))
            .init_resource::<ContentReportCell>()
            .insert_resource(ContentScanConfig {
                search_paths: self.search_paths.clone(),
                asset_root: self.asset_root.clone(),
            })
            .add_systems(Startup, start_content_scan)
            .add_systems(Update, mirror_content_scan);
    }
}

/// Spawns the package scan and detaches it. The task fills [`ContentIndexCell`], which wakes every
/// `alias://` read waiting on it, so nothing here needs polling or a state machine.
///
/// The scan builds its own reader over the default source. It must never read through `alias://`:
/// that would await an index only this task can fill, and hang.
fn start_content_scan(
    cell: Res<ContentIndexCell>,
    report: Res<ContentReportCell>,
    config: Res<ContentScanConfig>,
) {
    let cell = cell.0.clone();
    let report_cell = report.0.clone();
    let config = config.clone();
    IoTaskPool::get()
        .spawn(async move {
            info!("Scanning for packages in {:?}", config.search_paths);
            let mut make_reader = AssetSource::get_default_reader(config.asset_root);
            let vfs = AssetReaderVfs::new(make_reader());
            let scan = scan_packages(&vfs, &config.search_paths).await;
            let (index, report) = build_index(&scan);

            // The report goes first. Filling the index cell is what releases every `alias://`
            // read waiting on it, so anything that observes the index must already be able to
            // observe the report that explains it.
            let _ = report_cell.set(report).await;
            let _ = cell.set(index).await;
        })
        .detach();
}

/// Copies the finished scan into the `World` for the editor and the log.
///
/// `OnceCell::get` is a non-blocking read, so this is one cheap poll per frame until the scan
/// lands, and none after: the resource's own presence is the "already done" flag.
fn mirror_content_scan(
    mut commands: Commands,
    mirrored: Option<Res<ContentReport>>,
    index: Res<ContentIndexCell>,
    report: Res<ContentReportCell>,
) {
    if mirrored.is_some() {
        return;
    }
    let (Some(index), Some(report)) = (index.0.get(), report.0.get()) else {
        return;
    };
    commands.insert_resource(index.clone());
    commands.insert_resource(report.clone());
}
