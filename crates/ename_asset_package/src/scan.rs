//! Walking the search paths, reading every manifest found, and discovering the assets inside each
//! package.
//!
//! Nothing here fails the scan. An unreadable directory, an unparseable manifest, a broken
//! sidecar: each is recorded as a [`Problem`] and skipped, so one broken mod costs that mod and
//! nothing else. The problem list is the point -- "why is my asset not showing up" is the
//! question this system exists to answer. A missing search path is not a problem at all: a target
//! may list a `mods` directory a fresh install has not created, so that case is logged and passed
//! over without touching the list.
//!
//! `after`/`before`/`requires` ordering, the user constraint file and cross-package contested
//! aliases are phase 3. Packages are ordered by search path and then directory name, which is the
//! tiebreaker those constraints will sort on top of.

use crate::{
    AliasFile, AliasOrigin, CompiledRules, Manifest, RULES_FILE, Rules, Vfs, VfsError,
    alias_sidecar_target, vfs::BoxedFuture,
};
use std::{
    collections::BTreeMap,
    fmt::Display,
    path::{Path, PathBuf},
};
use tracing::{info, warn};
use uuid::Uuid;

/// The file that marks a directory as a package.
pub const MANIFEST_FILE: &str = "manifest.toml";

/// How many directories deep one package's asset walk may descend before it is cut off.
///
/// This is not a real limit on asset trees -- 64 is far deeper than any of them go. It exists so a
/// symlink cycle (a mod directory linking into itself, or two packages cross-linking shared art)
/// cannot recurse forever: both `Vfs` implementations report `is_dir` through calls that follow
/// symlinks, so `Walk::visit` cannot tell a cycle from a normal subdirectory. A visited set cannot
/// catch this either, because the relative path keeps growing instead of repeating. A depth cap is
/// the one check that is guaranteed to terminate.
const MAX_DEPTH: usize = 64;

/// One package found on disk: its parsed manifest, the directory it was found in, and every asset
/// discovered under it.
///
/// `root` and every asset `path` are relative to the vfs root, so a path can be handed straight to
/// the default `AssetReader`.
#[derive(Debug, Clone)]
pub struct Package {
    pub manifest: Manifest,
    pub root: PathBuf,
    pub assets: Vec<DiscoveredAsset>,
}

/// One addressable asset: the alias it claims, the file it is, and the identity tooling tracks it
/// by.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredAsset {
    /// Fully namespaced, straight out of the folder rule or the `.alias` file. This crate carries
    /// the string and never validates it: validation needs the alias *type*, which lives a layer
    /// away on purpose. `ename_asset_content` validates on the way into the index.
    pub alias: String,
    pub path: PathBuf,
    /// `None` until `xtask content fix` assigns one, in phase 4.
    pub guid: Option<Uuid>,
    pub origin: AliasOrigin,
}

/// What kind of thing went wrong. One flat list for the whole scan, because a user asking "what is
/// wrong with my content" wants one answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProblemKind {
    UnreadableDirectory,
    UnparseableManifest,
    UnparseableAliasFile,
    UnparseableRules,
    /// A `.alias` file whose asset is not there. The pair drifted apart.
    OrphanAliasFile,
    MissingGuid,
    /// Two files in one package claim one alias. Cross-*package* contests are phase 3.
    DuplicateAlias,
    /// An alias the alias layer rejected. Produced by `ename_asset_content`, never here: this
    /// crate must not name an alias type. The variant lives here so there is one list.
    InvalidAlias,
    /// The walk hit [`MAX_DEPTH`] and stopped descending. In practice this means a symlink cycle,
    /// since no real asset tree goes anywhere near that deep.
    DirectoryTooDeep,
}

impl Display for ProblemKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::UnreadableDirectory => "unreadable directory",
            Self::UnparseableManifest => "unparseable manifest",
            Self::UnparseableAliasFile => "unparseable .alias",
            Self::UnparseableRules => "unparseable _rules.toml",
            Self::OrphanAliasFile => "orphan .alias",
            Self::MissingGuid => "missing guid",
            Self::DuplicateAlias => "duplicate alias",
            Self::InvalidAlias => "invalid alias",
            Self::DirectoryTooDeep => "directory nested too deep",
        };
        f.write_str(text)
    }
}

/// Something the scan could not do, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub path: PathBuf,
    pub kind: ProblemKind,
    pub detail: String,
}

/// Everything one scan found.
#[derive(Debug, Default, Clone)]
pub struct Scan {
    /// In load order: search paths as the caller gave them, then directory name within each.
    pub packages: Vec<Package>,
    pub problems: Vec<Problem>,
}

/// Scans every search path for packages, in load order, discovering the assets in each.
///
/// A search path holds one directory per package. Package discovery does not recurse:
/// `basegame/core` is a package, `basegame/core/props` is not. *Asset* discovery inside a package
/// does recurse, all the way down.
pub async fn scan_packages(vfs: &dyn Vfs, search_paths: &[String]) -> Scan {
    let mut scan = Scan::default();
    for search_path in search_paths {
        read_packages_in(vfs, Path::new(search_path), &mut scan).await;
    }

    let assets: usize = scan.packages.iter().map(|p| p.assets.len()).sum();
    info!(
        "Found {} packages, {assets} assets, {} problems",
        scan.packages.len(),
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

        let mut walk = Walk::new(vfs);
        walk.visit(entry.path.clone(), None, 0).await;
        scan.problems.append(&mut walk.problems);
        scan.packages.push(Package {
            manifest,
            root: entry.path,
            assets: walk.assets,
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

/// One package's asset walk, accumulating as it descends.
///
/// A struct rather than a function returning a tuple because the walk is recursive: the claimed
/// aliases and the problems have to survive across every level, and threading four `&mut`s through
/// a boxed recursive future is worse than owning them.
struct Walk<'a> {
    vfs: &'a dyn Vfs,
    assets: Vec<DiscoveredAsset>,
    /// Alias to the file that won it. Only the winner's path is kept, to name it in the report.
    claimed: BTreeMap<String, PathBuf>,
    problems: Vec<Problem>,
}

impl<'a> Walk<'a> {
    fn new(vfs: &'a dyn Vfs) -> Self {
        Self {
            vfs,
            assets: Vec::new(),
            claimed: BTreeMap::new(),
            problems: Vec::new(),
        }
    }

    /// Visits one directory: adopt its rule if it has one, index its sidecars, discover its files,
    /// then descend. The future is boxed because it is recursive.
    ///
    /// `depth` is how many directories below the package root `dir` is; the root call is `0`. Past
    /// [`MAX_DEPTH`] the walk reports and returns without reading `dir` at all, which is what stops
    /// a symlink cycle from recursing forever.
    fn visit<'s>(
        &'s mut self,
        dir: PathBuf,
        inherited: Option<CompiledRules>,
        depth: usize,
    ) -> BoxedFuture<'s, ()> {
        Box::pin(async move {
            if depth > MAX_DEPTH {
                self.problems.push(Problem {
                    path: dir,
                    kind: ProblemKind::DirectoryTooDeep,
                    detail: format!("more than {MAX_DEPTH} directories deep; stopped descending"),
                });
                return;
            }

            let vfs = self.vfs;
            let entries = match vfs.read_dir(&dir).await {
                Ok(entries) => entries,
                Err(VfsError::NotFound(_)) => return,
                Err(err) => {
                    self.problems
                        .push(problem(&dir, ProblemKind::UnreadableDirectory, err));
                    return;
                }
            };

            // A `_rules.toml` here replaces the inherited rule for this directory and everything
            // below it. A broken one is reported and the inherited rule stays, which is what the
            // directory had before somebody added the broken file.
            let mut rules = inherited;
            let rules_path = dir.join(RULES_FILE);
            if entries.iter().any(|e| !e.is_dir && e.path == rules_path) {
                match read_rules(vfs, &rules_path, &dir).await {
                    Ok(compiled) => rules = Some(compiled),
                    Err(problem) => self.problems.push(problem),
                }
            }

            // Sidecars first, so every asset already knows whether one names it.
            let mut sidecars: BTreeMap<String, (PathBuf, AliasFile)> = BTreeMap::new();
            for entry in entries.iter().filter(|e| !e.is_dir) {
                let Some(name) = entry.path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let Some(target) = alias_sidecar_target(name) else {
                    continue;
                };
                match read_alias_file(vfs, &entry.path).await {
                    Ok(file) => {
                        sidecars.insert(target.to_owned(), (entry.path.clone(), file));
                    }
                    Err(problem) => self.problems.push(problem),
                }
            }

            for entry in entries.iter().filter(|e| !e.is_dir) {
                let Some(name) = entry.path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if is_sidecar(name) {
                    continue;
                }
                let sidecar = sidecars.remove(name).map(|(_, file)| file);
                self.discover(&entry.path, name, sidecar, rules.as_ref());
            }

            // Whatever is left named a file that is not here.
            for (target, (path, _)) in std::mem::take(&mut sidecars) {
                self.problems.push(Problem {
                    path,
                    kind: ProblemKind::OrphanAliasFile,
                    detail: format!("names `{target}`, which is not in this directory"),
                });
            }

            for entry in entries.iter().filter(|e| e.is_dir) {
                self.visit(entry.path.clone(), rules.clone(), depth + 1)
                    .await;
            }
        })
    }

    /// Decides whether one file is an asset, and under what alias.
    fn discover(
        &mut self,
        path: &Path,
        file_name: &str,
        sidecar: Option<AliasFile>,
        rules: Option<&CompiledRules>,
    ) {
        let authored = sidecar.as_ref().and_then(|s| s.alias.clone());

        // A sidecar's `include` beats the rule's patterns. With no explicit `include`, a sidecar
        // that names an alias is included anyway: somebody wrote that file on purpose, and making
        // them add `include = true` as well would be a rule nobody could guess.
        let included = match sidecar.as_ref().and_then(|s| s.include) {
            Some(explicit) => explicit,
            None if authored.is_some() => true,
            None => rules.is_some_and(|r| r.includes(file_name)),
        };
        if !included {
            return;
        }

        // No rule covers it and no sidecar names it. Skipped silently: one warning per README
        // would bury the warnings that matter.
        let Some(alias) = authored
            .clone()
            .or_else(|| rules.and_then(|r| r.alias_for(path)))
        else {
            return;
        };

        let origin = match (&authored, &sidecar) {
            (Some(_), Some(file)) => file.alias_origin,
            _ => AliasOrigin::Derived,
        };

        if sidecar.as_ref().is_some_and(|s| s.guid.is_none()) {
            self.problems.push(Problem {
                path: path.to_path_buf(),
                kind: ProblemKind::MissingGuid,
                detail: format!("`{alias}` has a .alias file with no guid"),
            });
        }

        if let Some(winner) = self.claimed.get(&alias) {
            self.problems.push(Problem {
                path: path.to_path_buf(),
                kind: ProblemKind::DuplicateAlias,
                detail: format!("`{alias}` is already claimed by {}", winner.display()),
            });
            return;
        }

        self.claimed.insert(alias.clone(), path.to_path_buf());
        self.assets.push(DiscoveredAsset {
            alias,
            path: path.to_path_buf(),
            guid: sidecar.and_then(|s| s.guid),
            origin,
        });
    }
}

/// A file that describes assets rather than being one.
fn is_sidecar(file_name: &str) -> bool {
    file_name == MANIFEST_FILE
        || file_name == RULES_FILE
        || alias_sidecar_target(file_name).is_some()
        // `Vfs` implementations already hide `.meta`. Belt and braces: a reader that did not
        // would otherwise turn every import setting into an asset.
        || Path::new(file_name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("meta"))
}

async fn read_rules(vfs: &dyn Vfs, path: &Path, dir: &Path) -> Result<CompiledRules, Problem> {
    let bytes = vfs
        .read_file(path)
        .await
        .map_err(|err| problem(path, ProblemKind::UnparseableRules, err))?;
    let rules: Rules = toml::from_str(&String::from_utf8_lossy(&bytes))
        .map_err(|err| problem(path, ProblemKind::UnparseableRules, err))?;
    CompiledRules::compile(rules, dir)
        .map_err(|err| problem(path, ProblemKind::UnparseableRules, err))
}

async fn read_alias_file(vfs: &dyn Vfs, path: &Path) -> Result<AliasFile, Problem> {
    let bytes = vfs
        .read_file(path)
        .await
        .map_err(|err| problem(path, ProblemKind::UnparseableAliasFile, err))?;
    toml::from_str(&String::from_utf8_lossy(&bytes))
        .map_err(|err| problem(path, ProblemKind::UnparseableAliasFile, err))
}
