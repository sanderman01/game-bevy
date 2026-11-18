//! Walking the search paths, reading every manifest found, and discovering the assets inside each
//! package.
//!
//! The asset discovery itself belongs to `ename_asset_alias` and is called once per package root.
//! What this file adds is the package: which directories are packages, what order they load in,
//! and the fact that a `package-info.toml` is not an asset.
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

use crate::{Disabled, LoadOrder, OrderEdges, PackageManifest, resolve, validate_package_id};
use ename_asset_alias::{DiscoveredAsset, IgnoredFiles, Problem, ProblemKind, scan_aliases};
use ename_asset_vfs::{Vfs, VfsError};
use serde::{Deserialize, Serialize};
use std::{
    fmt::Display,
    path::{Path, PathBuf},
};
use tracing::{info, warn};

/// The file that marks a directory as a package. Never an asset: it is added to the alias walk's
/// ignored names, or a folder rule matching `*` would turn every manifest into content.
pub const PACKAGE_INFO_FILE: &str = "package-info.toml";

/// One package found on disk: its parsed manifest, the directory it was found in, and every asset
/// discovered under it.
///
/// `root` and every asset `path` are relative to the vfs root, so a path can be handed straight to
/// the default `AssetReader`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub manifest: PackageManifest,
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

/// The packages on disk before load order is applied: what [`discover_packages`] found, and what a
/// build bakes to [`BAKED_DISCOVERY_FILE`] for a target that cannot list directories.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Discovery {
    /// In search path order, then directory name.
    pub packages: Vec<Package>,
    pub problems: Vec<Problem>,
}

/// Where a build leaves the baked [`Discovery`], relative to the asset root. The browser reads it
/// instead of walking the tree, because HTTP has no directory listings (`docs/web.md`).
pub const BAKED_DISCOVERY_FILE: &str = "content-discovery.ron.gz";

/// Why a baked [`Discovery`] could not be read.
#[derive(Debug, thiserror::Error)]
pub enum BakeError {
    #[error("gzip: {0}")]
    Gzip(std::io::Error),
    #[error(transparent)]
    Parse(#[from] ron::error::SpannedError),
    #[error(transparent)]
    Serialize(#[from] ron::Error),
}

impl Discovery {
    /// Compact RON, gzipped. A map of a few hundred thousand tiles is a 40 MB listing, and a
    /// static server is not obliged to compress it. Paths are written with `/` whichever OS baked
    /// it, so the file reads the same everywhere.
    pub fn encode(&self) -> Result<Vec<u8>, BakeError> {
        use std::io::Write;

        let mut portable = self.clone();
        for package in &mut portable.packages {
            portable_path(&mut package.root);
            portable_path(&mut package.search_path);
            for asset in &mut package.assets {
                portable_path(&mut asset.path);
            }
        }
        for problem in &mut portable.problems {
            portable_path(&mut problem.path);
        }
        let text = ron::to_string(&portable)?;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(text.as_bytes())
            .and_then(|()| encoder.finish())
            .map_err(BakeError::Gzip)
    }

    /// Reads what [`Discovery::encode`] wrote. Also takes it already decompressed, for a server
    /// that sent the file with `Content-Encoding: gzip` and let the browser unpack it.
    pub fn decode(bytes: &[u8]) -> Result<Self, BakeError> {
        use std::io::Read;

        const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
        let text = if bytes.starts_with(&GZIP_MAGIC) {
            let mut text = String::new();
            flate2::read::GzDecoder::new(bytes)
                .read_to_string(&mut text)
                .map_err(BakeError::Gzip)?;
            text
        } else {
            String::from_utf8_lossy(bytes).into_owned()
        };
        Ok(ron::from_str(&text)?)
    }
}

fn portable_path(path: &mut PathBuf) {
    if let Some(text) = path.to_str()
        && text.contains('\\')
    {
        *path = PathBuf::from(text.replace('\\', "/"));
    }
}

/// Scans every search path for packages, resolves their constraints, and discovers the assets in
/// each: [`discover_packages`] then [`resolve_discovery`].
///
/// A search path holds one directory per package. Package discovery does not recurse:
/// `basegame/core` is a package, `basegame/core/props` is not. *Asset* discovery inside a package
/// does recurse, all the way down.
///
/// The packages come back in the order the resolver settled on. Search path order and directory
/// name are underneath that, as the tiebreaker for whatever no constraint relates.
///
/// `ignored` is passed on to the alias walk in every package. [`PACKAGE_INFO_FILE`] is always added
/// to it.
pub async fn scan_packages(
    vfs: &dyn Vfs,
    search_paths: &[String],
    load_order: &LoadOrder,
    ignored: &IgnoredFiles,
) -> Scan {
    resolve_discovery(
        discover_packages(vfs, search_paths, ignored).await,
        load_order,
    )
}

/// Reads every package and its assets under `search_paths`, without ordering them.
pub async fn discover_packages(
    vfs: &dyn Vfs,
    search_paths: &[String],
    ignored: &IgnoredFiles,
) -> Discovery {
    let mut ignored = ignored.clone();
    ignored.file_names.push(PACKAGE_INFO_FILE.to_owned());

    let mut discovery = Discovery::default();
    for search_path in search_paths {
        read_packages_in(vfs, Path::new(search_path), &ignored, &mut discovery).await;
    }
    discovery
}

/// Puts what [`discover_packages`] found in load order, and logs the outcome.
pub fn resolve_discovery(discovery: Discovery, load_order: &LoadOrder) -> Scan {
    let resolution = resolve(&discovery.packages, load_order);
    // Moved, not cloned: a tiled map's package holds hundreds of thousands of assets.
    let mut found: Vec<Option<Package>> = discovery.packages.into_iter().map(Some).collect();
    let mut scan = Scan {
        packages: resolution
            .order
            .iter()
            .filter_map(|&i| found[i].take())
            .collect(),
        disabled: resolution.disabled,
        problems: discovery.problems,
        edges: resolution.edges,
    };
    scan.problems.extend(resolution.problems);

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
async fn read_packages_in(
    vfs: &dyn Vfs,
    search_path: &Path,
    ignored: &IgnoredFiles,
    scan: &mut Discovery,
) {
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
        let manifest_path = entry.path.join(PACKAGE_INFO_FILE);
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

        // A package with no `_alias_rules.toml` anywhere in its tree still gets every file
        // named, under its own package id -- the same as a rule written at its root would.
        let default_alias_template = format!("{}::{{stem}}", manifest.package.id);
        let mut found =
            scan_aliases(vfs, &entry.path, ignored, Some(&default_alias_template)).await;
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
async fn read_manifest(vfs: &dyn Vfs, path: &Path) -> Result<Option<PackageManifest>, Problem> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ename_asset_alias::AliasOrigin;

    fn discovery() -> Discovery {
        let manifest: PackageManifest = toml::from_str(
            "[package]\nid = \"core\"\nversion = \"1.0.0\"\nauthors = []\ntitle = \"\"\ndescription = \"\"",
        )
        .unwrap();
        let asset = |alias: &str, origin| DiscoveredAsset {
            alias: alias.to_owned(),
            path: PathBuf::from(format!("basegame\\core\\{alias}.png")),
            guid: None,
            origin,
        };
        Discovery {
            packages: vec![Package {
                manifest,
                root: "basegame/core".into(),
                search_path: "basegame".into(),
                assets: vec![
                    asset("core::a", AliasOrigin::Derived),
                    asset("core::b", AliasOrigin::Authored),
                ],
            }],
            problems: Vec::new(),
        }
    }

    #[test]
    fn a_bake_round_trips_gzipped_or_not() {
        let bytes = discovery().encode().unwrap();
        let decoded = Discovery::decode(&bytes).unwrap();
        let assets = &decoded.packages[0].assets;
        assert_eq!(assets[0].path, Path::new("basegame/core/core::a.png"));
        assert_eq!(assets[0].origin, AliasOrigin::Derived);
        assert_eq!(assets[1].origin, AliasOrigin::Authored);

        let mut plain = String::new();
        std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(&bytes[..]), &mut plain)
            .unwrap();
        assert!(!plain.contains("derived"), "the default origin is left out");
        assert_eq!(
            Discovery::decode(plain.as_bytes()).unwrap().packages[0].assets,
            decoded.packages[0].assets
        );
    }
}
