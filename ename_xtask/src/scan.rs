//! Running the same package scan `ename_asset_content` runs at startup, blocking, for a command
//! line tool that has neither an `IoTaskPool` nor an `AssetReader`.

use ename_asset_alias::IgnoredFiles;
use ename_asset_package::{LoadOrder, Problem, ProblemKind, Scan, scan_packages};
use ename_asset_vfs::{ArchiveVfs, Mounts};
use std::{path::Path, sync::Arc};

/// Scans `asset_root` for packages under `search_paths`, resolved against the load order at
/// `load_order_path`, with archives mounted. Returns the mounts too, so a subcommand can tell
/// which assets live inside an archive.
///
/// A missing `load_order_path` reads as no user constraints, same as `LoadOrder::read_from_path`
/// does for a missing file. Every subcommand in this crate starts here, so `ename_check`,
/// `ename_content_list` and `ename_fix` all see exactly the load order a running game would.
pub(crate) fn scan(
    asset_root: &Path,
    search_paths: &[String],
    load_order_path: Option<&Path>,
) -> (Scan, Arc<Mounts>) {
    let vfs = ArchiveVfs::new(asset_root);
    let (load_order, extra_problems) = read_load_order(load_order_path);
    let mut scan = futures_lite::future::block_on(scan_packages(
        &vfs,
        search_paths,
        &load_order,
        &IgnoredFiles::default(),
    ));
    scan.problems.extend(extra_problems);
    // The same fold `ename_asset_content` does, so `ename_check` fails on a broken archive.
    scan.problems
        .extend(vfs.mounts().problems().into_iter().map(|problem| Problem {
            path: problem.logical,
            kind: ProblemKind::Mount,
            detail: format!(
                "{} {}: {}",
                problem.kind,
                problem.physical.display(),
                problem.detail
            ),
        }));
    (scan, vfs.mounts().clone())
}

fn read_load_order(path: Option<&Path>) -> (LoadOrder, Vec<Problem>) {
    let Some(path) = path else {
        return (LoadOrder::default(), Vec::new());
    };
    match LoadOrder::read_from_path(path) {
        Ok(order) => (order, Vec::new()),
        Err(err) => (
            LoadOrder::default(),
            vec![Problem {
                path: path.to_path_buf(),
                kind: ProblemKind::UnparseableLoadOrder,
                detail: err.to_string(),
            }],
        ),
    }
}
