//! Walking a directory tree and working out which files are addressable, and under what alias.
//!
//! This is the whole of alias discovery: `_rules.toml` folder rules, `.alias` sidecars, and the
//! precedence between them. It knows nothing about packages, manifests or load order -- a caller
//! that has those concepts calls this once per package root and orders the results itself, and a
//! caller that does not points it at an asset root and is done.
//!
//! Nothing here fails the walk. An unreadable directory, a broken sidecar, a rule with a typo in
//! its template: each is recorded as a [`Problem`] and skipped, so one broken directory costs that
//! directory and nothing else. The problem list is the point -- "why is my asset not showing up"
//! is the question this system exists to answer.

use crate::{
    AliasFile, AliasOrigin, CompiledRules, RULES_FILE, Rules, Vfs, VfsError, alias_sidecar_target,
    is_rules_file, validate_alias, vfs::BoxedFuture,
};
use std::{
    collections::BTreeMap,
    fmt::Display,
    path::{Path, PathBuf},
};
use uuid::Uuid;

/// How many directories deep a walk may descend before it is cut off.
///
/// This is not a real limit on asset trees -- 64 is far deeper than any of them go. It exists so a
/// symlink cycle (a directory linking into itself, or two packages cross-linking shared art)
/// cannot recurse forever: both [`Vfs`] implementations report `is_dir` through calls that follow
/// symlinks, so [`Walk::visit`] cannot tell a cycle from a normal subdirectory. A visited set
/// cannot catch this either, because the relative path keeps growing instead of repeating. A depth
/// cap is the one check that is guaranteed to terminate.
const MAX_DEPTH: usize = 64;

/// One addressable asset: the alias it claims, the file it is, and the identity tooling tracks it
/// by.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredAsset {
    /// Fully namespaced, straight out of the folder rule or the `.alias` file, and already checked
    /// by [`validate_alias`] -- an asset whose alias could never resolve is a [`Problem`] instead
    /// of an entry here.
    pub alias: String,
    /// Relative to the vfs root, so it can be handed straight to an `AssetReader`.
    pub path: PathBuf,
    /// `None` until tooling assigns one.
    pub guid: Option<Uuid>,
    pub origin: AliasOrigin,
}

/// What kind of thing went wrong. One flat list, because a user asking "what is wrong with my
/// content" wants one answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProblemKind {
    UnreadableDirectory,
    UnparseableAliasFile,
    UnparseableRules,
    /// A `.alias` file whose asset is not there. The pair drifted apart.
    OrphanAliasFile,
    MissingGuid,
    /// Two files in one walk claim one alias.
    DuplicateAlias,
    /// An alias [`validate_alias`] rejected.
    InvalidAlias,
    /// The walk hit [`MAX_DEPTH`] and stopped descending. In practice this means a symlink cycle,
    /// since no real asset tree goes anywhere near that deep.
    DirectoryTooDeep,
    /// A `manifest.toml` that could not be read. Produced by `ename_asset_package`, never here:
    /// this crate has no concept of a package. The variant lives here so there is one list, the
    /// way `InvalidAlias` used to live a layer up for the mirror-image reason.
    UnparseableManifest,
}

impl Display for ProblemKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::UnreadableDirectory => "unreadable directory",
            Self::UnparseableAliasFile => "unparseable .alias",
            Self::UnparseableRules => "unparseable _rules.toml",
            Self::OrphanAliasFile => "orphan .alias",
            Self::MissingGuid => "missing guid",
            Self::DuplicateAlias => "duplicate alias",
            Self::InvalidAlias => "invalid alias",
            Self::DirectoryTooDeep => "directory nested too deep",
            Self::UnparseableManifest => "unparseable manifest",
        };
        f.write_str(text)
    }
}

/// Something a walk could not do, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub path: PathBuf,
    pub kind: ProblemKind,
    pub detail: String,
}

/// Everything one alias walk found.
///
/// Under the `bevy` feature this is also the resource [`crate::AliasScanPlugin`] mirrors into the
/// `World`, so a game can show its content problems without a second copy of the data.
#[derive(Debug, Default, Clone)]
#[cfg_attr(feature = "bevy", derive(bevy::ecs::resource::Resource))]
pub struct AliasScan {
    /// In walk order: sorted by path, directories after the files beside them.
    pub assets: Vec<DiscoveredAsset>,
    pub problems: Vec<Problem>,
}

impl AliasScan {
    /// Folds the discovered assets into an index, in the order they were found.
    ///
    /// A later claim on an alias replaces an earlier one, which is what makes an override work.
    /// Every alias here has already been validated, so nothing is dropped.
    pub fn to_index(&self) -> crate::ContentIndex {
        let mut index = crate::ContentIndex::default();
        for asset in &self.assets {
            let _ = index.insert(&asset.alias, &asset.path);
        }
        index
    }
}

/// Discovers every addressable asset under `root`.
///
/// `root` is relative to the vfs root; pass `""` for the whole tree. `ignored_file_names` names
/// files the caller's own format owns and discovery must never turn into an asset --
/// `ename_asset_package` passes `manifest.toml`. `_rules.toml`, `*.alias` and `*.meta` are always
/// ignored and need not be listed. Every one of those names is matched case-insensitively,
/// `ignored_file_names` included.
pub async fn scan_aliases(vfs: &dyn Vfs, root: &Path, ignored_file_names: &[&str]) -> AliasScan {
    let mut walk = Walk::new(vfs, ignored_file_names);
    walk.visit(root.to_path_buf(), None, 0).await;
    AliasScan {
        assets: walk.assets,
        problems: walk.problems,
    }
}

fn problem(path: &Path, kind: ProblemKind, detail: impl Display) -> Problem {
    Problem {
        path: path.to_path_buf(),
        kind,
        detail: detail.to_string(),
    }
}

/// One tree's asset walk, accumulating as it descends.
///
/// A struct rather than a function returning a tuple because the walk is recursive: the claimed
/// aliases and the problems have to survive across every level, and threading four `&mut`s through
/// a boxed recursive future is worse than owning them.
struct Walk<'a> {
    vfs: &'a dyn Vfs,
    ignored_file_names: &'a [&'a str],
    assets: Vec<DiscoveredAsset>,
    /// Alias to the file that won it. Only the winner's path is kept, to name it in the report.
    claimed: BTreeMap<String, PathBuf>,
    problems: Vec<Problem>,
}

impl<'a> Walk<'a> {
    fn new(vfs: &'a dyn Vfs, ignored_file_names: &'a [&'a str]) -> Self {
        Self {
            vfs,
            ignored_file_names,
            assets: Vec::new(),
            claimed: BTreeMap::new(),
            problems: Vec::new(),
        }
    }

    /// Visits one directory: adopt its rule if it has one, index its sidecars, discover its files,
    /// then descend. The future is boxed because it is recursive.
    ///
    /// `depth` is how many directories below the walk root `dir` is; the root call is `0`. Past
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
            if let Some(rules_path) = find_rules_file(&entries) {
                match read_rules(vfs, rules_path, &dir).await {
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
                if self.is_not_an_asset(name) {
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

    /// True for a file that describes assets rather than being one.
    ///
    /// Every comparison here is case-insensitive, because macOS and Windows preserve whatever case
    /// a file was saved in and a describing file mistaken for an asset becomes a phantom entry in
    /// the index under a name nobody chose.
    fn is_not_an_asset(&self, file_name: &str) -> bool {
        is_rules_file(file_name)
            || alias_sidecar_target(file_name).is_some()
            || self
                .ignored_file_names
                .iter()
                .any(|ignored| ignored.eq_ignore_ascii_case(file_name))
            // `Vfs` implementations already hide `.meta`. Belt and braces: a reader that did not
            // would otherwise turn every import setting into an asset.
            || Path::new(file_name)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("meta"))
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

        // Validation happens here rather than a layer up, because this crate owns the alias type.
        // An alias `AssetPath` would misread is not an asset with a problem, it is not an asset.
        if let Err(err) = validate_alias(&alias) {
            self.problems
                .push(problem(path, ProblemKind::InvalidAlias, err));
            return;
        }

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

/// The folder rule in one directory listing, if it has one.
///
/// [`RULES_FILE`] wins outright where a case-sensitive filesystem carries several spellings, so a
/// directory holding both `_rules.toml` and `_Rules.toml` behaves the way it does everywhere else.
fn find_rules_file(entries: &[crate::DirEntry]) -> Option<&Path> {
    let mut found: Option<&Path> = None;
    for entry in entries.iter().filter(|e| !e.is_dir) {
        let Some(name) = entry.path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !is_rules_file(name) {
            continue;
        }
        if name == RULES_FILE {
            return Some(&entry.path);
        }
        found.get_or_insert(&entry.path);
    }
    found
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
