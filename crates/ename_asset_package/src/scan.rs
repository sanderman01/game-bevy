//! Walking the search paths, reading every manifest found, and discovering the assets inside each
//! package.
//!
//! The asset discovery itself belongs to `ename_asset_alias` and is called once per package root.
//! What this file adds is the package: which directories are packages, what order they load in,
//! and the fact that a `manifest.toml` is not an asset.
//!
//! Nothing here fails the scan. An unreadable directory, an unparseable manifest, a broken
//! sidecar: each is recorded as a [`Problem`] and skipped, so one broken mod costs that mod and
//! nothing else. The problem list is the point -- "why is my asset not showing up" is the
//! question this system exists to answer. A missing search path is not a problem at all: a target
//! may list a `mods` directory a fresh install has not created, so that case is logged and passed
//! over without touching the list.
//!
//! The order the packages come back in is the resolver's, not the walk's: search path and then
//! directory name is only the baseline `resolve` sorts its constraints on top of.

use crate::{Disabled, LoadOrder, Manifest, OrderEdges, resolve, validate_package_id};
use ename_asset_alias::{DiscoveredAsset, Problem, ProblemKind, Vfs, VfsError, scan_aliases};
use std::{
    fmt::Display,
    path::{Path, PathBuf},
};
use tracing::{info, warn};

/// The file that marks a directory as a package. Never an asset: it is passed to the alias walk as
/// a name to ignore, or a folder rule matching `*` would turn every manifest into content.
pub const MANIFEST_FILE: &str = "manifest.toml";

/// One package found on disk: its parsed manifest, the directory it was found in, and every asset
/// discovered under it.
///
/// `root` and every asset `path` are relative to the vfs root, so a path can be handed straight to
/// the default `AssetReader`.
#[derive(Debug, Clone)]
pub struct Package {
    pub manifest: Manifest,
    pub root: PathBuf,
    /// The search path this package was found under, as the caller wrote it. Kept because it is
    /// half the answer to "why did this one win": two packages in different search paths were
    /// ordered by the target's list, and two in the same one by directory name.
    pub search_path: PathBuf,
    pub assets: Vec<DiscoveredAsset>,
}

/// Everything one scan found.
#[derive(Debug, Default, Clone)]
pub struct Scan {
    /// In **resolved** load order: constraints first, and search path then directory name
    /// underneath them. Packages the resolver disabled are not here.
    pub packages: Vec<Package>,
    /// Found on disk and deliberately not loaded, each with the reason.
    pub disabled: Vec<Disabled>,
    pub problems: Vec<Problem>,
    /// What ordered the packages, kept so contest reporting can explain a winner.
    pub edges: OrderEdges,
}

/// Scans every search path for packages, resolves their constraints, and discovers the assets in
/// each.
///
/// A search path holds one directory per package. Package discovery does not recurse:
/// `basegame/core` is a package, `basegame/core/props` is not. *Asset* discovery inside a package
/// does recurse, all the way down.
///
/// The packages come back in the order the resolver settled on. Search path order and directory
/// name are underneath that, as the tiebreaker for whatever no constraint relates.
pub async fn scan_packages(vfs: &dyn Vfs, search_paths: &[String], load_order: &LoadOrder) -> Scan {
    let mut scan = Scan::default();
    for search_path in search_paths {
        read_packages_in(vfs, Path::new(search_path), &mut scan).await;
    }

    let resolution = resolve(&scan.packages, load_order);
    let found = std::mem::take(&mut scan.packages);
    // A clone per package rather than a `swap_remove` dance. This list is dozens of entries long
    // and reading straight is worth more than the allocation.
    scan.packages = resolution.order.iter().map(|i| found[*i].clone()).collect();
    scan.disabled = resolution.disabled;
    scan.problems.extend(resolution.problems);
    scan.edges = resolution.edges;

    let assets: usize = scan.packages.iter().map(|p| p.assets.len()).sum();
    info!(
        "Found {} packages, {assets} assets, {} disabled, {} problems",
        scan.packages.len(),
        scan.disabled.len(),
        scan.problems.len()
    );
    for disabled in &scan.disabled {
        warn!("  -- {} disabled: {}", disabled.id, disabled.reason);
    }
    for problem in &scan.problems {
        warn!(
            "  !! {} at {}: {}",
            problem.kind,
            problem.path.display(),
            problem.detail
        );
    }
    scan
}

/// Reads every package directly inside `search_path`.
async fn read_packages_in(vfs: &dyn Vfs, search_path: &Path, scan: &mut Scan) {
    let entries = match vfs.read_dir(search_path).await {
        Ok(entries) => entries,
        // Not an error: a target may list a `mods` directory that a fresh install has not created.
        Err(VfsError::NotFound(_)) => {
            warn!("Package search path not found: {}", search_path.display());
            return;
        }
        Err(err) => {
            scan.problems.push(Problem {
                path: search_path.to_path_buf(),
                kind: ProblemKind::UnreadableDirectory,
                detail: err.to_string(),
            });
            return;
        }
    };

    // `Vfs::read_dir` returns entries sorted by path, and directory name is the tiebreaker the
    // whole load order rests on. Without that guarantee the order differs between machines and the
    // bug shows up as an override that works for one person.
    for entry in entries.into_iter().filter(|e| e.is_dir) {
        let manifest_path = entry.path.join(MANIFEST_FILE);
        let manifest = match read_manifest(vfs, &manifest_path).await {
            Ok(Some(manifest)) => manifest,
            // No manifest: this directory is simply not a package.
            Ok(None) => continue,
            Err(problem) => {
                scan.problems.push(problem);
                continue;
            }
        };

        if let Err(err) = validate_package_id(&manifest.package.id) {
            scan.problems
                .push(problem(&manifest_path, ProblemKind::InvalidPackageId, err));
            continue;
        }

        let mut found = scan_aliases(vfs, &entry.path, &[MANIFEST_FILE]).await;
        scan.problems.append(&mut found.problems);
        scan.packages.push(Package {
            manifest,
            root: entry.path,
            search_path: search_path.to_path_buf(),
            assets: found.assets,
        });
    }
}

/// `Ok(None)` means "no manifest here", which is not a problem. `Err` means there was one and it
/// was unusable.
async fn read_manifest(vfs: &dyn Vfs, path: &Path) -> Result<Option<Manifest>, Problem> {
    let bytes = match vfs.read_file(path).await {
        Ok(bytes) => bytes,
        Err(VfsError::NotFound(_)) => return Ok(None),
        Err(err) => return Err(problem(path, ProblemKind::UnparseableManifest, err)),
    };
    toml::from_str(&String::from_utf8_lossy(&bytes))
        .map(Some)
        .map_err(|err| problem(path, ProblemKind::UnparseableManifest, err))
}

fn problem(path: &Path, kind: ProblemKind, detail: impl Display) -> Problem {
    Problem {
        path: path.to_path_buf(),
        kind,
        detail: detail.to_string(),
    }
}
