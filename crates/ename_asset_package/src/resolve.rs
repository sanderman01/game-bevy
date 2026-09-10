//! Turning constraints into a load order.
//!
//! Pure: packages in baseline order and the user's constraints in, a load order out, with every
//! package it disabled and every reason recorded. No filesystem and no `App`, which is what lets
//! the whole of it be stated as values in a test.
//!
//! Three things happen, in this order, and the order matters:
//!
//! 1. **`requires` is checked**, and a package whose requirement is absent or the wrong version is
//!    disabled. Disabling cascades to whatever required it, to a fixpoint, because a package whose
//!    dependency is gone is in exactly the situation `requires` exists to prevent.
//! 2. **Constraints become edges** over what is left. A user constraint that contradicts a
//!    manifest one wins and the manifest's edge is dropped, with a [`Problem`] recording it.
//! 3. **The edges are topologically sorted**, with the incoming order as the tiebreaker whenever
//!    more than one package could go next. An unconstrained set therefore comes out exactly as it
//!    went in, and the baseline breaks every tie the constraints leave open. A cycle disables its
//!    members, and whatever is ordered behind one goes with them.
//!
//! Nothing here fails. The spec calls a cycle a hard error, and in a command line tool it is one:
//! `xtask content check` turns these problems into a non-zero exit. In the running game a hard
//! error would mean one broken mod costing the whole session, which is the failure mode the
//! per-package problem list exists to avoid.

use crate::{LoadOrder, Package, Requirement, Version};
use ename_asset_alias::{Problem, ProblemKind};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
    fmt::Display,
    path::PathBuf,
};

/// Who asked for one package to load before another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstraintSource {
    /// The manifest of the named package, in its `after` or `before`.
    Manifest(String),
    /// The user's load order file, which beats any manifest that disagrees.
    User,
}

impl Display for ConstraintSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Manifest(id) => write!(f, "{id}'s manifest"),
            Self::User => f.write_str("the user's load order"),
        }
    }
}

/// Which package must load before which, after the user has overruled whatever they disagreed
/// with.
///
/// Kept past the sort because contest reporting needs it: "bigships won because it says it loads
/// after core" and "bigships won because its directory name sorts later" are different answers,
/// and only one of them is worth acting on.
#[derive(Debug, Default, Clone)]
pub struct OrderEdges {
    /// `earlier -> later -> who said so`.
    edges: BTreeMap<String, BTreeMap<String, ConstraintSource>>,
    /// Transitive closure of `edges`, computed once when the resolver finishes.
    reachable: BTreeMap<String, BTreeSet<String>>,
}

impl OrderEdges {
    /// The constraint that directly orders `earlier` before `later`, if one does.
    pub fn direct(&self, earlier: &str, later: &str) -> Option<&ConstraintSource> {
        self.edges.get(earlier)?.get(later)
    }

    /// Whether anything at all orders `earlier` before `later`, directly or through a chain.
    pub fn ordered(&self, earlier: &str, later: &str) -> bool {
        self.reachable
            .get(earlier)
            .is_some_and(|set| set.contains(later))
    }

    /// The constraints running between the members of one loop, each with whoever wrote it.
    ///
    /// Naming the packages is not enough to fix a loop: a user constraint can close one over
    /// manifests that are all individually correct, and then a list of package ids sends the
    /// reader to three files none of which is at fault. The source is the half that points at the
    /// file to edit.
    fn describe_loop(&self, members: &[String]) -> String {
        let members: BTreeSet<&str> = members.iter().map(String::as_str).collect();
        self.edges
            .iter()
            .filter(|(earlier, _)| members.contains(earlier.as_str()))
            .flat_map(|(earlier, laters)| {
                laters
                    .iter()
                    .filter(|(later, _)| members.contains(later.as_str()))
                    .map(move |(later, source)| format!("{later} after {earlier} [{source}]"))
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn insert(&mut self, earlier: &str, later: &str, source: ConstraintSource) {
        self.edges
            .entry(earlier.to_owned())
            .or_default()
            .insert(later.to_owned(), source);
    }

    /// Floyd-Warshall over ids, run once the edge set is complete. The package count is in the
    /// dozens, so the cubic term is nothing and a closure computed once beats a search per
    /// contested alias.
    ///
    /// The closure is also how a cycle is identified: an id that reaches itself is on one.
    fn close(&mut self) {
        let ids: Vec<String> = self
            .edges
            .iter()
            .flat_map(|(from, tos)| std::iter::once(from.clone()).chain(tos.keys().cloned()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        let mut reachable: BTreeMap<String, BTreeSet<String>> = self
            .edges
            .iter()
            .map(|(from, tos)| (from.clone(), tos.keys().cloned().collect()))
            .collect();

        for k in &ids {
            let through = reachable.get(k).cloned().unwrap_or_default();
            for i in &ids {
                if reachable.get(i).is_some_and(|set| set.contains(k)) {
                    reachable
                        .entry(i.clone())
                        .or_default()
                        .extend(through.iter().cloned());
                }
            }
        }
        self.reachable = reachable;
    }
}

/// Why a package is not in the load order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisableReason {
    /// `requires` named something absent, or present at a version it does not accept.
    Unsatisfied {
        requirement: Requirement,
        /// The version that was found, or `None` when the package is not installed at all.
        found: Option<Version>,
    },
    /// Required a package that was itself disabled.
    RequirementDisabled { id: String },
    /// Part of an `after`/`before` loop. Every member is disabled, because any order the resolver
    /// picked would be one nobody asked for.
    ///
    /// `members` is this package's own loop. A second, unrelated loop elsewhere in the scan is a
    /// separate problem with a separate fix, and listing its packages here would send this
    /// author reading manifests that have nothing to do with them.
    Cycle { members: Vec<String> },
    /// Ordered after a cycle without being in one. It cannot be placed either -- nothing can go
    /// after a package that never loads -- but the constraints to edit are inside the cycle.
    ///
    /// Worth a variant of its own because the reason is the whole point: telling an author their
    /// package is *in* a loop sends them looking through their own `after` and `before` for a
    /// loop that is not there.
    BehindCycle { cycle: Vec<String> },
}

impl Display for DisableReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsatisfied {
                requirement,
                found: Some(found),
            } => write!(f, "requires {requirement}, found {found}"),
            Self::Unsatisfied {
                requirement,
                found: None,
            } => write!(f, "requires {requirement}, which is not installed"),
            Self::RequirementDisabled { id } => write!(f, "requires {id}, which is disabled"),
            Self::Cycle { members } => write!(f, "in a load order cycle: {}", members.join(" -> ")),
            Self::BehindCycle { cycle } => {
                write!(f, "loads after a load order cycle: {}", cycle.join(" -> "))
            }
        }
    }
}

/// A package that was found on disk and will not be loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disabled {
    pub id: String,
    pub root: PathBuf,
    pub reason: DisableReason,
}

/// What the resolver decided.
#[derive(Debug, Default, Clone)]
pub struct Resolution {
    /// Indices into the input slice, in load order. Disabled packages are absent.
    pub order: Vec<usize>,
    pub disabled: Vec<Disabled>,
    pub problems: Vec<Problem>,
    pub edges: OrderEdges,
}

/// Puts `packages` in load order, disabling whatever cannot load.
///
/// `packages` arrives in the baseline order -- search path as the target listed them, then
/// directory name -- and that order is the tiebreaker underneath every constraint.
pub fn resolve(packages: &[Package], load_order: &LoadOrder) -> Resolution {
    let mut resolution = Resolution::default();

    let mut enabled = vec![true; packages.len()];
    check_requirements(packages, &mut enabled, &mut resolution);

    let live: Vec<usize> = (0..packages.len()).filter(|i| enabled[*i]).collect();
    let index_of: BTreeMap<&str, usize> = live
        .iter()
        .map(|i| (packages[*i].manifest.package.id.as_str(), *i))
        .collect();

    collect_constraints(packages, load_order, &live, &index_of, &mut resolution);
    // Before the sort, not after: the sort needs the closure to tell a cycle's members from the
    // packages merely stuck behind it.
    resolution.edges.close();
    sort_and_report_cycles(packages, &live, &index_of, &mut resolution);

    resolution
}

/// Disables every package whose `requires` cannot be met, to a fixpoint.
///
/// The fixpoint is what makes disabling cascade: a package that required something disabled in an
/// earlier pass is itself disabled in a later one, and so on until nothing changes.
fn check_requirements(packages: &[Package], enabled: &mut [bool], resolution: &mut Resolution) {
    let versions: BTreeMap<&str, &Version> = packages
        .iter()
        .map(|p| (p.manifest.package.id.as_str(), &p.manifest.package.version))
        .collect();

    loop {
        let mut changed = false;
        for (i, package) in packages.iter().enumerate() {
            if !enabled[i] {
                continue;
            }
            let info = &package.manifest.package;
            for requirement in &info.requires {
                let found = versions.get(requirement.id.as_str()).copied();
                let satisfied = packages.iter().enumerate().any(|(j, other)| {
                    let other = &other.manifest.package;
                    enabled[j] && requirement.matches(&other.id, &other.version)
                });
                // The package exists in the input but no *enabled* copy of it is left, so this is
                // a cascade rather than a version mismatch and deserves to say so.
                let dependency_disabled = found.is_some()
                    && !packages.iter().enumerate().any(|(j, other)| {
                        enabled[j] && other.manifest.package.id == requirement.id
                    });

                let reason = if satisfied {
                    None
                } else if dependency_disabled {
                    Some(DisableReason::RequirementDisabled {
                        id: requirement.id.clone(),
                    })
                } else {
                    Some(DisableReason::Unsatisfied {
                        requirement: requirement.clone(),
                        found: found.cloned(),
                    })
                };

                let Some(reason) = reason else { continue };

                enabled[i] = false;
                changed = true;
                resolution.problems.push(Problem {
                    path: package.root.clone(),
                    kind: ProblemKind::UnsatisfiedRequirement,
                    detail: format!("{}: {reason}", info.id),
                });
                resolution.disabled.push(Disabled {
                    id: info.id.clone(),
                    root: package.root.clone(),
                    reason,
                });
                break;
            }
        }
        if !changed {
            return;
        }
    }
}

/// Turns the user's constraints and every live manifest's into [`OrderEdges`], and reports the
/// manifest constraints the user contradicted.
fn collect_constraints(
    packages: &[Package],
    load_order: &LoadOrder,
    live: &[usize],
    index_of: &BTreeMap<&str, usize>,
    resolution: &mut Resolution,
) {
    let mut user_edges: BTreeSet<(String, String)> = BTreeSet::new();
    for constraint in &load_order.constraints {
        if !index_of.contains_key(constraint.package.as_str()) {
            continue;
        }
        for earlier in &constraint.after {
            if index_of.contains_key(earlier.as_str()) {
                user_edges.insert((earlier.clone(), constraint.package.clone()));
            }
        }
        for later in &constraint.before {
            if index_of.contains_key(later.as_str()) {
                user_edges.insert((constraint.package.clone(), later.clone()));
            }
        }
    }
    for (earlier, later) in &user_edges {
        resolution
            .edges
            .insert(earlier, later, ConstraintSource::User);
    }

    for i in live {
        let package = &packages[*i];
        let info = &package.manifest.package;
        let manifest_edges = info
            .after
            .iter()
            .map(|earlier| (earlier.as_str(), info.id.as_str()))
            .chain(
                info.before
                    .iter()
                    .map(|later| (info.id.as_str(), later.as_str())),
            );

        for (earlier, later) in manifest_edges {
            if !index_of.contains_key(earlier) || !index_of.contains_key(later) {
                // An ordering hint about a package nobody installed. Not a failure: use
                // `requires` to say a package must be there.
                continue;
            }
            if user_edges.contains(&(later.to_owned(), earlier.to_owned())) {
                resolution.problems.push(Problem {
                    path: package.root.clone(),
                    kind: ProblemKind::OverruledConstraint,
                    detail: format!(
                        "{}: `{earlier}` before `{later}` was dropped; the user's load order says \
                         the opposite",
                        info.id
                    ),
                });
                continue;
            }
            resolution
                .edges
                .insert(earlier, later, ConstraintSource::Manifest(info.id.clone()));
        }
    }
}

/// Kahn's algorithm, with the baseline order as the ready-set tiebreaker, then a cycle report for
/// whatever it could not place.
///
/// A `BinaryHeap` of `Reverse` indices pops the earliest-in-baseline package that is ready, so an
/// unconstrained set comes out exactly as it went in.
///
/// Expects [`OrderEdges::close`] to have run: the reachability it computed is what separates a
/// cycle's members from the packages stuck behind them.
fn sort_and_report_cycles(
    packages: &[Package],
    live: &[usize],
    index_of: &BTreeMap<&str, usize>,
    resolution: &mut Resolution,
) {
    let mut in_degree: BTreeMap<usize, usize> = live.iter().map(|i| (*i, 0)).collect();
    let mut successors: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (earlier, laters) in &resolution.edges.edges {
        let Some(from) = index_of.get(earlier.as_str()) else {
            continue;
        };
        for later in laters.keys() {
            let Some(to) = index_of.get(later.as_str()) else {
                continue;
            };
            successors.entry(*from).or_default().push(*to);
            *in_degree.entry(*to).or_default() += 1;
        }
    }

    let mut ready: BinaryHeap<Reverse<usize>> = in_degree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(i, _)| Reverse(*i))
        .collect();

    while let Some(Reverse(i)) = ready.pop() {
        resolution.order.push(i);
        for successor in successors.get(&i).cloned().unwrap_or_default() {
            let degree = in_degree.entry(successor).or_default();
            *degree -= 1;
            if *degree == 0 {
                ready.push(Reverse(successor));
            }
        }
    }

    // Whatever the sort could not place is in a cycle or behind one.
    if resolution.order.len() == live.len() {
        return;
    }

    let placed: BTreeSet<usize> = resolution.order.iter().copied().collect();
    // An id that reaches itself is on a loop; one that is merely stuck is downstream of somebody
    // else's. The two get different reasons because they need different fixes.
    let (in_cycle, behind): (Vec<usize>, Vec<usize>) = live
        .iter()
        .copied()
        .filter(|i| !placed.contains(i))
        .partition(|i| {
            let id = &packages[*i].manifest.package.id;
            resolution.edges.ordered(id, id)
        });

    // Two loops in one scan are two problems with two separate fixes, so each stuck package is
    // sorted into the one it is actually on. Reaching each other both ways is an equivalence
    // relation, which is why comparing against a single member of each group is enough.
    let mut loops: Vec<Vec<usize>> = Vec::new();
    for i in in_cycle {
        let id = &packages[i].manifest.package.id;
        let same_loop = loops.iter_mut().find(|members| {
            let other = &packages[members[0]].manifest.package.id;
            resolution.edges.ordered(other, id) && resolution.edges.ordered(id, other)
        });
        match same_loop {
            Some(members) => members.push(i),
            None => loops.push(vec![i]),
        }
    }

    for members in &loops {
        let ids: Vec<String> = members
            .iter()
            .map(|i| packages[*i].manifest.package.id.clone())
            .collect();
        resolution.problems.push(Problem {
            path: packages[members[0]].root.clone(),
            kind: ProblemKind::DependencyCycle,
            detail: format!(
                "these constraints form a loop: {}; none of these packages will load: {}",
                resolution.edges.describe_loop(&ids),
                ids.join(", ")
            ),
        });
        for i in members {
            resolution.disabled.push(Disabled {
                id: packages[*i].manifest.package.id.clone(),
                root: packages[*i].root.clone(),
                reason: DisableReason::Cycle {
                    members: ids.clone(),
                },
            });
        }
    }

    let cycle_members: Vec<&str> = loops
        .iter()
        .flatten()
        .map(|i| packages[*i].manifest.package.id.as_str())
        .collect();
    for i in behind {
        let id = &packages[i].manifest.package.id;
        // Only the members that actually hold this one back, so a second unrelated cycle
        // elsewhere in the scan does not turn up in its reason.
        let cycle: Vec<String> = cycle_members
            .iter()
            .filter(|member| resolution.edges.ordered(member, id))
            .map(|member| (*member).to_owned())
            .collect();
        resolution.disabled.push(Disabled {
            id: id.clone(),
            root: packages[i].root.clone(),
            reason: DisableReason::BehindCycle { cycle },
        });
    }
}
