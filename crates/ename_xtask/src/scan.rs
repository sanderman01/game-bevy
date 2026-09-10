//! Running the same package scan `ename_asset_content` runs at startup, blocking, for a command
//! line tool that has neither an `IoTaskPool` nor an `AssetReader`.

use ename_asset_alias::StdVfs;
use ename_asset_package::{LoadOrder, Problem, ProblemKind, Scan, scan_packages};
use std::path::Path;

/// Scans `asset_root` for packages under `search_paths`, resolved against the load order at
/// `load_order_path`.
///
/// A missing `load_order_path` reads as no user constraints, same as `LoadOrder::read_from_path`
/// does for a missing file. Every subcommand in this crate starts here, so `ename_check`,
/// `ename_list` and `ename_fix` all see exactly the load order a running game would.
pub(crate) fn scan(
    asset_root: &Path,
    search_paths: &[String],
    load_order_path: Option<&Path>,
) -> Scan {
    let vfs = StdVfs::new(asset_root);
    let (load_order, extra_problems) = read_load_order(load_order_path);
    let mut scan = futures_lite::future::block_on(scan_packages(&vfs, search_paths, &load_order));
    scan.problems.extend(extra_problems);
    scan
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
