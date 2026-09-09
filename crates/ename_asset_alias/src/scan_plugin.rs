//! Filling the alias index by scanning the asset root, with no other crate involved.
//!
//! [`AliasSourcePlugin`](crate::AliasSourcePlugin) registers the `alias://` source and the cell it
//! reads through, and deliberately does not fill that cell. This plugin is the default answer to
//! who fills it: walk the asset root, take every alias a `_rules.toml` or a `.alias` file names,
//! and hand the result over. Adding it is all a consumer of this crate has to do to load
//! `alias://core::airship` off a tree they wrote by hand.
//!
//! It is a separate plugin rather than a flag so that a project with its own idea of load order
//! composes instead of opts out. `ename_asset_content` scans package by package, in an order the
//! manifests decide, and simply never adds this plugin -- there is nothing for it to switch off.

use crate::{AliasScan, ContentIndexCell, discovery::scan_aliases, reader::AliasSourcePlugin};
use async_lock::OnceCell;
use bevy::{
    app::{App, Plugin, Startup, Update},
    asset::io::AssetSource,
    ecs::{
        resource::Resource,
        system::{Commands, Res},
    },
    log::{info, warn},
    tasks::IoTaskPool,
};
use std::{path::Path, sync::Arc};

/// Relative path to the asset root. Mirrors `AssetPlugin::file_path`'s default.
const DEFAULT_ASSET_ROOT: &str = "assets";

/// Scans the asset root on startup and fills the index the `alias://` source waits on.
///
/// Requires [`AliasSourcePlugin`] to have been added first, which is what owns the cell this
/// fills. [`AliasPlugins`](crate::AliasPlugins) adds both in the right order.
pub struct AliasScanPlugin {
    asset_root: String,
    root: String,
    ignored_file_names: Vec<String>,
}

impl Default for AliasScanPlugin {
    fn default() -> Self {
        Self {
            asset_root: DEFAULT_ASSET_ROOT.to_owned(),
            root: String::new(),
            ignored_file_names: Vec::new(),
        }
    }
}

impl AliasScanPlugin {
    /// Sets the asset root the scan reads through. Must match `AssetPlugin::file_path`, and
    /// `AliasSourcePlugin::with_asset_root`.
    pub fn with_asset_root(mut self, path: impl Into<String>) -> Self {
        self.asset_root = path.into();
        self
    }

    /// Limits the scan to one directory below the asset root. Empty, the default, scans all of it.
    pub fn with_root(mut self, path: impl Into<String>) -> Self {
        self.root = path.into();
        self
    }

    /// Names files the walk must never turn into an asset, on top of `_rules.toml`, `*.alias` and
    /// `*.meta`, which it always skips. For a project whose asset tree carries a file of its own
    /// that a permissive folder rule would otherwise sweep up.
    pub fn with_ignored_file_names<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.ignored_file_names = names.into_iter().map(Into::into).collect();
        self
    }
}

/// What [`start_alias_scan`] needs, kept out of the plugin so the system can read it.
#[derive(Resource, Clone)]
struct AliasScanConfig {
    asset_root: String,
    root: String,
    ignored_file_names: Vec<String>,
}

/// Where [`start_alias_scan`] leaves the scan for [`mirror_alias_scan`] to pick up.
///
/// Private: the scan reaches the rest of the world as `Res<AliasScan>`, and a second way to read
/// it would be a second thing to keep in step.
#[derive(Resource, Clone, Default)]
struct AliasScanCell(Arc<OnceCell<AliasScan>>);

impl Plugin for AliasScanPlugin {
    fn build(&self, app: &mut App) {
        // The cell this fills belongs to `AliasSourcePlugin`. Adding it here instead would give
        // the reader one cell and the scan another, and every `alias://` load would wait forever
        // on a cell nothing fills.
        assert!(
            app.is_plugin_added::<AliasSourcePlugin>(),
            "AliasScanPlugin must be added after AliasSourcePlugin, which owns the index cell it \
             fills. Add `AliasPlugins` to get both in the right order."
        );

        app.init_resource::<AliasScanCell>()
            .insert_resource(AliasScanConfig {
                asset_root: self.asset_root.clone(),
                root: self.root.clone(),
                ignored_file_names: self.ignored_file_names.clone(),
            })
            .add_systems(Startup, start_alias_scan)
            .add_systems(Update, mirror_alias_scan);
    }
}

/// Spawns the walk and detaches it. The task fills [`ContentIndexCell`], which wakes every
/// `alias://` read waiting on it, so nothing here needs polling or a state machine.
///
/// The scan builds its own reader over the default source. It must never read through `alias://`:
/// that would await an index only this task can fill, and hang.
fn start_alias_scan(
    cell: Res<ContentIndexCell>,
    scan: Res<AliasScanCell>,
    config: Res<AliasScanConfig>,
) {
    let index_cell = cell.0.clone();
    let scan_cell = scan.0.clone();
    let config = config.clone();
    IoTaskPool::get()
        .spawn(async move {
            info!("Scanning {:?} for aliases", config.asset_root);
            let mut make_reader = AssetSource::get_default_reader(config.asset_root);
            let vfs = crate::AssetReaderVfs::new(make_reader());
            let ignored: Vec<&str> = config
                .ignored_file_names
                .iter()
                .map(String::as_str)
                .collect();
            let scan = scan_aliases(&vfs, Path::new(&config.root), &ignored).await;

            info!(
                "Found {} aliases, {} problems",
                scan.assets.len(),
                scan.problems.len()
            );
            for problem in &scan.problems {
                warn!(
                    "  !! {} at {}: {}",
                    problem.kind,
                    problem.path.display(),
                    problem.detail
                );
            }

            // The scan goes first. Filling the index cell is what releases every `alias://` read
            // waiting on it, so anything that observes the index must already be able to observe
            // the scan that explains it.
            let index = scan.to_index();
            let _ = scan_cell.set(scan).await;
            let _ = index_cell.set(index).await;
        })
        .detach();
}

/// Copies the finished scan into the `World`, so a game can show what it found and what it could
/// not.
///
/// `OnceCell::get` is a non-blocking read, so this is one cheap poll per frame until the scan
/// lands, and none after: the resource's own presence is the "already done" flag.
fn mirror_alias_scan(
    mut commands: Commands,
    mirrored: Option<Res<AliasScan>>,
    scan: Res<AliasScanCell>,
) {
    if mirrored.is_some() {
        return;
    }
    let Some(scan) = scan.0.get() else {
        return;
    };
    commands.insert_resource(scan.clone());
}
