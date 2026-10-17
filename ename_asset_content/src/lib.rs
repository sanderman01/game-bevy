//! `ename_asset_content` -- the glue between `ename_asset_alias` and `ename_asset_package`.
//!
//! The two crates below this one are deliberately unaware of each other: one knows what an alias
//! is, the other knows what a package is, and neither needs the other to be useful. This crate is
//! where they meet. It is also where the plugin lives, because deciding what goes in an `App` is
//! composition, not asset logic. See `docs/design/crate-layout.md`.
//!
//! With the `zip` or `tar` feature it also mounts archives under the `Default` asset source, and
//! folds their problems into the [`ContentReport`]. See `docs/assets-archives.md`.

// The door a consumer opens by depending on this crate alone: enough to read a `ContentReport`
// -- every type its fields name, down to the version requirement inside a `Requirement` -- and to
// write or hand in a `LoadOrder`. The fold that produces the report is deliberately not here: a
// caller that wants `build_index` is building content rather than an `App`, and that caller
// belongs on `ename_asset_package`, which owns it.
pub use ename_asset_alias::{ContentIndex, Problem, ProblemKind};
pub use ename_asset_package::{
    ConstraintSource, ContentReport, ContestReason, ContestedAlias, DisableReason, Disabled,
    LOAD_ORDER_FILE, LoadOrder, LoadOrderError, PackageRef, PackageSummary, Requirement, Tiebreak,
    UserConstraint, Version, VersionReq,
};

use async_lock::OnceCell;
use bevy::{
    app::{App, Plugin, Startup, Update},
    asset::AssetServer,
    ecs::{
        resource::Resource,
        system::{Commands, Res},
    },
    log::{info, warn},
    tasks::IoTaskPool,
};
use ename_asset_alias::{AliasSourcePlugin, ContentIndexCell, IgnoredFiles};
use ename_asset_package::{build_index, scan_packages};
#[cfg(any(feature = "zip", feature = "tar"))]
use ename_asset_vfs::ArchiveSourcePlugin;
use ename_asset_vfs::AssetReaderVfs;
/// Re-exported so the engine's stage save can refuse a write into an archive. Lives with the
/// mounts, so it works whichever crate turned an archive format on.
pub use ename_asset_vfs::check_writable;
use ename_asset_vfs::{ArchiveMounts, MountProblem};
use std::{path::PathBuf, sync::Arc};

/// The default asset root, matching `AssetPlugin::file_path`'s own default. Exported so a
/// caller that needs an absolute filesystem path for something outside the asset pipeline (for
/// example, writing a stage file directly to disk) does not have to duplicate the literal.
pub const DEFAULT_ASSET_ROOT: &str = "assets";

/// No search paths by default. Which directories a game ships is game policy: the target passes
/// them in.
const DEFAULT_SEARCH_PATHS: &[&str] = &[];

/// Adds the `alias://` asset source and the startup scan that fills its index, and with the `zip`
/// or `tar` feature, [`ArchiveSourcePlugin`](ename_asset_vfs::ArchiveSourcePlugin).
///
/// **Must be added before `AssetPlugin`.** `ename_engine` does that with
/// `add_before::<AssetPlugin>`, and `AliasSourcePlugin` asserts on it.
pub struct AssetContentPlugin {
    search_paths: Vec<String>,
    asset_root: String,
    /// Where the user's load order file is, if the target has one. `None` means no user
    /// constraints, which is the normal case on a fresh install and the only case in a test that
    /// does not care.
    load_order_file: Option<PathBuf>,
    ignored: IgnoredFiles,
}

impl Default for AssetContentPlugin {
    fn default() -> Self {
        Self {
            search_paths: DEFAULT_SEARCH_PATHS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            asset_root: DEFAULT_ASSET_ROOT.to_owned(),
            load_order_file: None,
            ignored: IgnoredFiles::default(),
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

    /// Sets the asset root. Must match `AssetPlugin::file_path`. Reads go through the `Default`
    /// source; this is the physical directory behind it, for live reload.
    pub fn with_asset_root(mut self, path: impl Into<String>) -> Self {
        self.asset_root = path.into();
        self
    }

    /// Sets the user's load order file. Outside the asset tree by design: it is the player's
    /// file, not the game's content, so it is read with `std::fs` through a path the target
    /// supplies rather than through the asset reader.
    ///
    /// Which directory that path lives in is not this crate's concern: `ename_engine` defaults it
    /// to the project's persistent data directory; a test passes a fixture path.
    pub fn with_load_order_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.load_order_file = Some(path.into());
        self
    }

    /// Names files the scan must never turn into an asset, on top of `manifest.toml`,
    /// `_alias_rules.toml`, `*.alias` and `*.meta`, which it always skips.
    pub fn with_ignored_file_names<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.ignored.file_names = names.into_iter().map(Into::into).collect();
        self
    }

    /// Replaces the sidecar extensions the scan skips, without the dot. Defaults to
    /// [`DEFAULT_IGNORED_SIDECAR_EXTENSIONS`](ename_asset_alias::DEFAULT_IGNORED_SIDECAR_EXTENSIONS).
    pub fn with_ignored_sidecar_extensions<I, S>(mut self, extensions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.ignored.sidecar_extensions = extensions.into_iter().map(Into::into).collect();
        self
    }
}

/// What [`start_content_scan`] needs, kept out of the plugin so the system can read it.
#[derive(Resource, Clone)]
struct ContentScanConfig {
    search_paths: Vec<String>,
    load_order_file: Option<PathBuf>,
    ignored: IgnoredFiles,
}

/// Where [`start_content_scan`] leaves the report for [`mirror_content_scan`] to pick up.
///
/// Private: the report reaches the rest of the world as `Res<ContentReport>`, and a second way to
/// read it would be a second thing to keep in step.
#[derive(Resource, Clone, Default)]
struct ContentReportCell(Arc<OnceCell<ContentReport>>);

impl Plugin for AssetContentPlugin {
    fn build(&self, app: &mut App) {
        // A target that registers the archive source itself keeps its own.
        #[cfg(any(feature = "zip", feature = "tar"))]
        if !app.is_plugin_added::<ArchiveSourcePlugin>() {
            app.add_plugins(
                ArchiveSourcePlugin::default().with_asset_root(self.asset_root.clone()),
            );
        }
        app.add_plugins(AliasSourcePlugin::default().with_asset_root(self.asset_root.clone()))
            .init_resource::<ContentReportCell>()
            .insert_resource(ContentScanConfig {
                search_paths: self.search_paths.clone(),
                load_order_file: self.load_order_file.clone(),
                ignored: self.ignored.clone(),
            })
            .add_systems(Startup, start_content_scan)
            .add_systems(Update, mirror_content_scan);
    }
}

/// Spawns the package scan and detaches it. The task fills [`ContentIndexCell`], which wakes every
/// `alias://` read waiting on it, so nothing here needs polling or a state machine.
///
/// The scan reads the `Default` source. It must never read through `alias://`: that would await an
/// index only this task can fill, and hang.
fn start_content_scan(
    server: Res<AssetServer>,
    cell: Res<ContentIndexCell>,
    report: Res<ContentReportCell>,
    config: Res<ContentScanConfig>,
    mounts: Option<Res<ArchiveMounts>>,
) {
    let mounts = mounts.map(|mounts| mounts.0.clone());
    let cell = cell.0.clone();
    let report_cell = report.0.clone();
    let config = config.clone();
    let vfs = AssetReaderVfs::new(server.clone());
    IoTaskPool::get()
        .spawn(async move {
            info!("Scanning for packages in {:?}", config.search_paths);
            // Reading the file happens here rather than in `Startup` because it is blocking io,
            // and the io pool is where blocking io belongs.
            let mut problems = Vec::new();
            let load_order = match &config.load_order_file {
                Some(path) => match LoadOrder::read_from_path(path) {
                    Ok(order) => order,
                    Err(err) => {
                        warn!("Ignoring the user load order file: {err}");
                        problems.push(Problem {
                            path: path.clone(),
                            kind: ProblemKind::UnparseableLoadOrder,
                            detail: err.to_string(),
                        });
                        LoadOrder::default()
                    }
                },
                None => LoadOrder::default(),
            };

            let scan =
                scan_packages(&vfs, &config.search_paths, &load_order, &config.ignored).await;
            let (index, mut report) = build_index(&scan);
            report.problems.extend(problems);
            // Mounts the scan passed through have been classified and indexed by now.
            // Problems found later, by a read, are only logged.
            if let Some(mounts) = &mounts {
                report
                    .problems
                    .extend(mounts.problems().iter().map(mount_problem));
            }

            // The one place the finished report is listed. `build_index` narrates the fold as it
            // happens, interleaved with a line per package; this is the block at the end of the
            // log that answers "which mod won what, and did anyone choose that".
            if !report.contests.is_empty() {
                info!("{} contested aliases:", report.contests.len());
                for contest in &report.contests {
                    info!("  {contest}");
                }
            }

            // The report goes first. Filling the index cell is what releases every `alias://`
            // read waiting on it, so anything that observes the index must already be able to
            // observe the report that explains it.
            let _ = report_cell.set(report).await;
            let _ = cell.set(index).await;
        })
        .detach();
}

/// Copies the finished scan out of the task's cells and into the `World`, as `ContentIndex` and
/// `ContentReport` resources.
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

fn mount_problem(problem: &MountProblem) -> Problem {
    Problem {
        path: problem.logical.clone(),
        kind: ProblemKind::Mount,
        detail: format!(
            "{} {}: {}",
            problem.kind,
            problem.physical.display(),
            problem.detail
        ),
    }
}
