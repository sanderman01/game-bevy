//! Walking the search paths and reading every manifest found.
//!
//! Phase 1 reads the existing `manifest.toml` format and orders packages by search path, then by
//! directory name. `_rules.toml`, the `after`/`before`/`requires` constraints that sort ahead of
//! that, and the user constraint file land in phase 3; see
//! `scratch/content-addressing-design.md`.
//!
//! Nothing here fails the scan. A missing search path, an unreadable directory or an unparseable
//! manifest is logged and skipped, so one broken mod costs that mod and nothing else.

use crate::Manifest;
use bevy::{
    asset::io::{AssetReaderError, ErasedAssetReader},
    log::{error, info, warn},
    tasks::futures_lite::{AsyncReadExt, StreamExt},
};
use std::path::{Path, PathBuf};

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
pub async fn scan_packages(
    reader: &dyn ErasedAssetReader,
    search_paths: &[String],
) -> Vec<Package> {
    let mut packages = Vec::new();
    for search_path in search_paths {
        packages.extend(read_packages_in(reader, Path::new(search_path)).await);
    }
    info!("Found {} packages", packages.len());
    packages
}

/// Reads every package directly inside `search_path`.
async fn read_packages_in(reader: &dyn ErasedAssetReader, search_path: &Path) -> Vec<Package> {
    let mut entries = match reader.read_directory(search_path).await {
        Ok(entries) => entries,
        Err(AssetReaderError::NotFound(_)) => {
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

    let mut directories = Vec::new();
    while let Some(entry) = entries.next().await {
        if matches!(reader.is_directory(&entry).await, Ok(true)) {
            directories.push(entry);
        }
    }

    // `read_directory` yields in whatever order the platform gives, and directory name is the
    // tiebreaker the whole ordering rests on. Without this sort the load order differs between
    // machines and the bug shows up as an override that works for one person.
    directories.sort();

    let mut packages = Vec::new();
    for entry in directories {
        let manifest_path = entry.join(MANIFEST_FILE);
        match read_manifest(reader, &manifest_path).await {
            Ok(manifest) => packages.push(Package {
                manifest,
                root: entry,
            }),
            Err(AssetReaderError::NotFound(_)) => {}
            Err(err) => error!("Failed to read {}: {err}", manifest_path.display()),
        }
    }
    packages
}

/// Reads and parses one manifest. A parse failure is logged here and reported as `NotFound`, which
/// the caller already treats as "this directory is not a package".
async fn read_manifest(
    reader: &dyn ErasedAssetReader,
    path: &Path,
) -> Result<Manifest, AssetReaderError> {
    let mut bytes = Vec::new();
    reader.read(path).await?.read_to_end(&mut bytes).await?;
    let text = String::from_utf8_lossy(&bytes);
    toml::from_str(&text).map_err(|err| {
        error!("Failed to parse {}: {err}", path.display());
        AssetReaderError::NotFound(path.to_path_buf())
    })
}
