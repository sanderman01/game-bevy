//! Folding a resolved scan into one index, and saying what happened on the way.
//!
//! One pass, deliberately. The index, the contested aliases and the `removes` are three views of
//! the same walk over the ordered packages, and computing them separately is two implementations
//! of load order that will disagree the first time one of them changes.
//!
//! It lives here rather than a layer up because `xtask content check` has to produce exactly these
//! contests with no Bevy in its dependency graph.

use crate::{
    ConstraintSource, ContestReason, ContestedAlias, Disabled, PackageRef, Scan, Tiebreak, Version,
};
use ename_asset_alias::{ContentIndex, Problem, ProblemKind};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
use tracing::{info, warn};

/// One package that made it into the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSummary {
    pub id: String,
    pub version: Version,
    pub root: PathBuf,
    /// Aliases this package actually registered, which is its discovered assets minus whatever
    /// was rejected.
    pub aliases: usize,
}

/// Everything one content scan found, problems included.
///
/// One list of problems rather than several, because "what is wrong with my content" is one
/// question. Contested aliases are separate: a contest is usually not a problem, it is the
/// override system working, and burying the two together would make neither readable.
#[derive(Debug, Default, Clone)]
#[cfg_attr(feature = "bevy", derive(bevy::ecs::resource::Resource))]
pub struct ContentReport {
    /// In load order.
    pub packages: Vec<PackageSummary>,
    /// Found on disk and deliberately not loaded.
    pub disabled: Vec<Disabled>,
    /// Every alias more than one package claimed, and why the winner won.
    pub contests: Vec<ContestedAlias>,
    pub problems: Vec<Problem>,
}

/// Folds a resolved scan into one index, and reports what happened.
///
/// The packages arrive in load order, so a later claim on an alias replaces an earlier one -- that
/// replacement is what an override *is*, and every one of them becomes a [`ContestedAlias`].
///
/// A rejected alias is reported and skipped. One malformed entry must not cost the rest of the
/// package, and one malformed package must not cost the game.
pub fn build_index(scan: &Scan) -> (ContentIndex, ContentReport) {
    let mut index = ContentIndex::default();
    let mut report = ContentReport {
        packages: Vec::new(),
        disabled: scan.disabled.clone(),
        contests: Vec::new(),
        problems: scan.problems.clone(),
    };

    // Who currently holds each alias, by index into `scan.packages`, so a later claim knows who
    // it beat. `ContentIndex` remembers the path but not the package, and the package is the half
    // a report line is about.
    let mut holders: BTreeMap<String, usize> = BTreeMap::new();
    // Everyone who has ever held each alias, kept beside `holders` rather than folded into it
    // because the two answer different questions. Who holds it *now* is who a contest names as the
    // loser; who has held it is what an `overrides` declaration is judged against. A `removes`
    // clears the first and leaves the second alone: the override still happened.
    let mut claimants: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();

    for (i, package) in scan.packages.iter().enumerate() {
        let info = &package.manifest.package;
        info!("  {:20} {}", info.id, package.root.display());

        // `removes` runs before this package's own claims, so a package may remove an alias and
        // claim it back in one step, and a package after it may claim it again. Removal is a step
        // in load order, not a ban.
        for alias in &info.removes {
            if index.remove(alias).is_some() {
                holders.remove(alias);
                info!("  -- {alias:24} removed by {}", info.id);
            } else {
                let detail = format!(
                    "{} removes `{alias}`, which no package before it provides",
                    info.id
                );
                warn!("  !! {detail}");
                report.problems.push(Problem {
                    path: package.root.clone(),
                    kind: ProblemKind::DeadRemoval,
                    detail,
                });
            }
        }

        let mut aliases = 0;
        // Sets, so overriding six of one package's aliases is one statement about that package
        // rather than six copies of the same warning. `overridden` is every package this one took
        // an alias from; `undeclared` is the subset it took one from without saying so.
        let mut overridden: BTreeSet<String> = BTreeSet::new();
        let mut undeclared: BTreeSet<String> = BTreeSet::new();
        for asset in &package.assets {
            match index.insert(&asset.alias, &asset.path) {
                Ok(_) => {
                    aliases += 1;
                    if let Some(&holder) = holders.get(&asset.alias)
                        && holder != i
                    {
                        report.contests.push(contest(scan, &asset.alias, i, holder));
                    }

                    let claimed = claimants.entry(asset.alias.clone()).or_default();
                    let previous: Vec<&str> = claimed
                        .iter()
                        .filter(|held| **held != i)
                        .map(|held| scan.packages[*held].manifest.package.id.as_str())
                        .collect();
                    // Intent is judged per alias. Naming any of an alias's previous holders says
                    // this package meant to take that alias; a third mod that happened to touch it
                    // first does not make the declaration dishonest, and warning about it would
                    // fire on correctly written manifests as soon as two mods change one thing.
                    if !previous
                        .iter()
                        .any(|id| info.overrides.iter().any(|declared| declared == id))
                    {
                        undeclared.extend(previous.iter().map(|id| (*id).to_owned()));
                    }
                    overridden.extend(previous.iter().map(|id| (*id).to_owned()));
                    claimed.insert(i);
                    holders.insert(asset.alias.clone(), i);
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

        // Intent, both ways round: a collision this package never declared, and a declaration that
        // collided with nothing. The second is what catches a misspelled alias, which is otherwise
        // completely silent -- the alias is valid, it simply names nobody.
        for id in &undeclared {
            let detail = format!(
                "{} overrides an alias of `{id}` without naming it in `overrides`",
                info.id
            );
            warn!("  !! {detail}");
            report.problems.push(Problem {
                path: package.root.clone(),
                kind: ProblemKind::UndeclaredOverride,
                detail,
            });
        }
        for id in &info.overrides {
            if !overridden.contains(id) {
                let detail = format!(
                    "{} declares `overrides = [\"{id}\"]` but claims none of its aliases",
                    info.id
                );
                warn!("  !! {detail}");
                report.problems.push(Problem {
                    path: package.root.clone(),
                    kind: ProblemKind::DeadOverride,
                    detail,
                });
            }
        }

        report.packages.push(PackageSummary {
            id: info.id.clone(),
            version: info.version.clone(),
            root: package.root.clone(),
            aliases,
        });
    }

    info!(
        "Registered {} aliases from {} packages, {} contested",
        index.len(),
        scan.packages.len(),
        report.contests.len()
    );
    // The contests themselves are not listed here. They are in the returned report, and the
    // caller lists them once it has the whole of it -- the fold is not the last thing that can
    // add to a report, so a listing from inside it is a listing of something not yet finished.
    (index, report)
}

/// Works out why `winner` beat `loser` for `alias`.
fn contest(scan: &Scan, alias: &str, winner: usize, loser: usize) -> ContestedAlias {
    let winner_pkg = &scan.packages[winner];
    let loser_pkg = &scan.packages[loser];
    let winner_id = &winner_pkg.manifest.package.id;
    let loser_id = &loser_pkg.manifest.package.id;

    let reason = match scan.edges.direct(loser_id, winner_id) {
        Some(source) => ContestReason::Ordered {
            source: source.clone(),
            direct: true,
        },
        None if scan.edges.ordered(loser_id, winner_id) => ContestReason::Ordered {
            // The chain that ordered them is not one constraint, so name the winner's own manifest
            // as the closest thing a reader can go and look at.
            source: ConstraintSource::Manifest(winner_id.clone()),
            direct: false,
        },
        None => ContestReason::Unordered {
            tiebreak: if winner_pkg.search_path == loser_pkg.search_path {
                Tiebreak::DirectoryName {
                    search_path: winner_pkg.search_path.clone(),
                }
            } else {
                Tiebreak::SearchPath {
                    winner: winner_pkg.search_path.clone(),
                    loser: loser_pkg.search_path.clone(),
                }
            },
        },
    };

    ContestedAlias {
        alias: alias.to_owned(),
        winner: PackageRef {
            id: winner_id.clone(),
            version: winner_pkg.manifest.package.version.clone(),
        },
        loser: PackageRef {
            id: loser_id.clone(),
            version: loser_pkg.manifest.package.version.clone(),
        },
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::build_index;
    use crate::{Manifest, Package, Scan};
    use ename_asset_alias::{AliasOrigin, DiscoveredAsset, Problem, ProblemKind};
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
        let root = PathBuf::from(root);
        Package {
            manifest: manifest(id),
            search_path: root.parent().unwrap_or(Path::new("")).to_path_buf(),
            root,
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
            ..Scan::default()
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
            ..Scan::default()
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
            ..Scan::default()
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
            problems: vec![Problem {
                path: PathBuf::from("base/core/gone.glb.alias"),
                kind: ProblemKind::OrphanAliasFile,
                detail: String::new(),
            }],
            ..Scan::default()
        };

        let (_, report) = build_index(&scan);
        assert_eq!(report.problems.len(), 1);
        assert_eq!(report.problems[0].kind, ProblemKind::OrphanAliasFile);
    }
}
