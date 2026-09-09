//! Folding a scan into the alias index.
//!
//! This is the whole reason the crate exists. `ename_asset_package` carries alias *strings* it
//! read out of files and deliberately cannot validate them, because validation needs the alias
//! type and that lives in the other leaf. This is the only place both are in scope.

use crate::report::{ContentReport, PackageSummary};
use bevy::log::{info, warn};
use ename_asset_alias::ContentIndex;
use ename_asset_package::{Problem, ProblemKind, Scan};

/// Folds a scan into one index, and reports what happened.
///
/// Order is load order: a later package's claim on an alias replaces an earlier one's, which is
/// how a mod overrides the base game. Phase 3 turns each of those replacements into a contested
/// alias record explaining *why* the winner won; today it is a log line.
///
/// A rejected alias is reported and skipped. One malformed entry must not cost the rest of the
/// package, and one malformed package must not cost the game.
pub fn build_index(scan: &Scan) -> (ContentIndex, ContentReport) {
    let mut index = ContentIndex::default();
    let mut report = ContentReport {
        packages: Vec::new(),
        problems: scan.problems.clone(),
    };

    for package in &scan.packages {
        let info = &package.manifest.package;
        info!("  {:20} {}", info.id, package.root.display());

        let mut aliases = 0;
        for asset in &package.assets {
            match index.insert(&asset.alias, &asset.path) {
                Ok(previous) => {
                    aliases += 1;
                    match previous {
                        Some(previous) => info!(
                            "  ++ {:24} {} (was {})",
                            asset.alias,
                            asset.path.display(),
                            previous.display()
                        ),
                        None => info!("  ++ {:24} {}", asset.alias, asset.path.display()),
                    }
                }
                Err(err) => {
                    warn!("  !! {} declares an invalid alias: {err}", info.id);
                    report.problems.push(Problem {
                        path: asset.path.clone(),
                        kind: ProblemKind::InvalidAlias,
                        detail: err.to_string(),
                    });
                }
            }
        }

        report.packages.push(PackageSummary {
            id: info.id.clone(),
            version: info.version,
            root: package.root.clone(),
            aliases,
        });
    }

    info!(
        "Registered {} aliases from {} packages",
        index.len(),
        scan.packages.len()
    );
    (index, report)
}

#[cfg(test)]
mod tests {
    use super::build_index;
    use ename_asset_package::{
        AliasOrigin, DiscoveredAsset, Manifest, Package, Problem, ProblemKind, Scan,
    };
    use std::path::{Path, PathBuf};

    fn manifest(id: &str) -> Manifest {
        toml::from_str(&format!(
            r#"
            [package]
            id = "{id}"
            version = "1.0.0"
            authors = []
            title = "{id}"
            description = ""
            "#
        ))
        .expect("test manifest parses")
    }

    fn asset(alias: &str, path: &str) -> DiscoveredAsset {
        DiscoveredAsset {
            alias: alias.to_owned(),
            path: PathBuf::from(path),
            guid: None,
            origin: AliasOrigin::Derived,
        }
    }

    fn package(id: &str, root: &str, assets: Vec<DiscoveredAsset>) -> Package {
        Package {
            manifest: manifest(id),
            root: PathBuf::from(root),
            assets,
        }
    }

    #[test]
    fn a_discovered_asset_lands_in_the_index_under_its_own_alias() {
        let scan = Scan {
            packages: vec![package(
                "core",
                "base/core",
                vec![asset("core::airship", "base/core/airship.glb")],
            )],
            problems: Vec::new(),
        };

        let (index, report) = build_index(&scan);
        assert_eq!(
            index.resolve("core::airship"),
            Some(Path::new("base/core/airship.glb"))
        );
        assert_eq!(report.packages[0].aliases, 1);
    }

    /// A later package claiming an alias an earlier one already had is the override, and it is the
    /// reason the whole system exists.
    #[test]
    fn a_later_package_claiming_an_alias_wins_it() {
        let scan = Scan {
            packages: vec![
                package(
                    "core",
                    "base/core",
                    vec![asset("core::airship", "base/core/airship.glb")],
                ),
                package(
                    "bigships",
                    "mods/bigships",
                    vec![asset("core::airship", "mods/bigships/big.glb")],
                ),
            ],
            problems: Vec::new(),
        };

        let (index, _) = build_index(&scan);
        assert_eq!(
            index.resolve("core::airship"),
            Some(Path::new("mods/bigships/big.glb"))
        );
    }

    /// One malformed entry must not cost the rest of the package, and it must be reported rather
    /// than only logged.
    #[test]
    fn an_invalid_alias_is_reported_and_the_rest_still_registers() {
        let scan = Scan {
            packages: vec![package(
                "core",
                "base/core",
                vec![
                    asset("core::air#ship", "base/core/airship.glb"),
                    asset("core::map", "base/core/map.glb"),
                ],
            )],
            problems: Vec::new(),
        };

        let (index, report) = build_index(&scan);
        assert_eq!(index.len(), 1);
        assert_eq!(
            index.resolve("core::map"),
            Some(Path::new("base/core/map.glb"))
        );
        assert_eq!(
            report.problems.iter().map(|p| p.kind).collect::<Vec<_>>(),
            [ProblemKind::InvalidAlias]
        );
        assert_eq!(report.packages[0].aliases, 1);
    }

    /// The scan's own problems travel through untouched, so a user gets one list.
    #[test]
    fn the_scans_problems_are_carried_into_the_report() {
        let scan = Scan {
            packages: Vec::new(),
            problems: vec![Problem {
                path: PathBuf::from("base/core/gone.glb.alias"),
                kind: ProblemKind::OrphanAliasFile,
                detail: String::new(),
            }],
        };

        let (_, report) = build_index(&scan);
        assert_eq!(report.problems.len(), 1);
        assert_eq!(report.problems[0].kind, ProblemKind::OrphanAliasFile);
    }
}
