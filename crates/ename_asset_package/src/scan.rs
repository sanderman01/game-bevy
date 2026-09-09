//! Walking the search paths and reading every manifest found.
//!
//! Phase 1 reads the existing `manifest.toml` format and orders packages by search path, then by
//! directory name. `_rules.toml`, the `after`/`before`/`requires` constraints that sort ahead of
//! that, and the user constraint file land in phase 3; see
//! `scratch/content-addressing-design.md`.
//!
//! Nothing here fails the scan. A missing search path, an unreadable directory or an unparseable
//! manifest is logged and skipped, so one broken mod costs that mod and nothing else.

use crate::{Manifest, Vfs, VfsError};
use std::path::{Path, PathBuf};
use tracing::{error, info, warn};

/// The file that marks a directory as a package.
pub const MANIFEST_FILE: &str = "manifest.toml";

/// One package found on disk: its parsed manifest and the directory it was found in.
///
/// `root` is relative to the asset root, so joining a manifest's relative asset path onto it gives
/// a path the default `AssetReader` can open.
#[derive(Debug, Clone)]
pub struct Package {
    pub manifest: Manifest,
    pub root: PathBuf,
}

/// Scans every search path for packages, in load order.
///
/// A search path holds one directory per package. This does not recurse further: `basegame/core`
/// is a package, `basegame/core/props` is not.
///
/// Load order is the search paths in the order the caller gave them, then directory name within
/// each. Phase 3 topologically sorts declared `after`/`before` constraints ahead of this and
/// leaves it as the tiebreaker for whatever no constraint relates. See
/// `scratch/content-addressing-design.md`.
pub async fn scan_packages(vfs: &dyn Vfs, search_paths: &[String]) -> Vec<Package> {
    let mut packages = Vec::new();
    for search_path in search_paths {
        packages.extend(read_packages_in(vfs, Path::new(search_path)).await);
    }
    info!("Found {} packages", packages.len());
    packages
}

/// Reads every package directly inside `search_path`.
async fn read_packages_in(vfs: &dyn Vfs, search_path: &Path) -> Vec<Package> {
    let entries = match vfs.read_dir(search_path).await {
        Ok(entries) => entries,
        Err(VfsError::NotFound(_)) => {
            warn!("Package search path not found: {}", search_path.display());
            return Vec::new();
        }
        Err(err) => {
            error!(
                "Failed to read search path {}: {err}",
                search_path.display()
            );
            return Vec::new();
        }
    };

    let mut packages = Vec::new();
    // `Vfs::read_dir` returns entries sorted by path, and directory name is the tiebreaker the
    // whole load order rests on. Without that guarantee the order differs between machines and
    // the bug shows up as an override that works for one person.
    for entry in entries.into_iter().filter(|e| e.is_dir) {
        let manifest_path = entry.path.join(MANIFEST_FILE);
        match read_manifest(vfs, &manifest_path).await {
            Ok(manifest) => packages.push(Package {
                manifest,
                root: entry.path,
            }),
            Err(VfsError::NotFound(_)) => {}
            Err(err) => error!("Failed to read {}: {err}", manifest_path.display()),
        }
    }
    packages
}

/// Reads and parses one manifest. A parse failure is logged here and reported as `NotFound`, which
/// the caller already treats as "this directory is not a package".
async fn read_manifest(vfs: &dyn Vfs, path: &Path) -> Result<Manifest, VfsError> {
    let bytes = vfs.read_file(path).await?;
    let text = String::from_utf8_lossy(&bytes);
    toml::from_str(&text).map_err(|err| {
        error!("Failed to parse {}: {err}", path.display());
        VfsError::NotFound(path.to_path_buf())
    })
}
