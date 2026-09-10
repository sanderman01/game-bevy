//! The resolver, over hand-built packages.
//!
//! No filesystem and no `App`: `resolve` is a pure function from a baseline-ordered package list
//! and a user load order to a load order, so every rule in it can be stated as a value in and a
//! value out. The tests that prove the same rules survive a real directory tree are in
//! `package_scan.rs`.

use ename_asset_alias::AliasOrigin;
use ename_asset_package::{
    ContestReason, DisableReason, DiscoveredAsset, LoadOrder, Manifest, Package, ProblemKind, Scan,
    Tiebreak, Version, build_index, resolve,
};
use std::path::PathBuf;

/// Builds a package with the given id and constraint fields. `extra` is TOML appended to the
/// `[package]` table, so a test reads like the manifest an author would write.
fn package(search_path: &str, dir: &str, extra: &str) -> Package {
    let manifest: Manifest = toml::from_str(&format!(
        r#"
        [package]
        id = "{dir}"
        version = "1.0.0"
        authors = []
        title = "{dir}"
        description = ""
        {extra}
        "#
    ))
    .expect("the test manifest parses");

    Package {
        manifest,
        root: PathBuf::from(format!("{search_path}/{dir}")),
        search_path: PathBuf::from(search_path),
        assets: Vec::new(),
    }
}

/// The ids `resolve` put in load order.
fn order<'a>(packages: &'a [Package], load_order: &LoadOrder) -> Vec<&'a str> {
    resolve(packages, load_order)
        .order
        .into_iter()
        .map(|i| packages[i].manifest.package.id.as_str())
        .collect()
}

/// With nothing to relate them, the resolved order is exactly the order that came in -- search
/// path first, directory name second. That baseline is what breaks every tie the constraints
/// leave open.
#[test]
fn unconstrained_packages_keep_the_order_they_came_in() {
    let packages = [
        package("base", "core", ""),
        package("mods", "apple", ""),
        package("mods", "zebra", ""),
    ];
    assert_eq!(
        order(&packages, &LoadOrder::default()),
        ["core", "apple", "zebra"]
    );
}

#[test]
fn after_moves_a_package_later() {
    let packages = [
        package("mods", "apple", r#"after = ["zebra"]"#),
        package("mods", "zebra", ""),
    ];
    assert_eq!(order(&packages, &LoadOrder::default()), ["zebra", "apple"]);
}

#[test]
fn before_moves_a_package_earlier() {
    let packages = [
        package("mods", "apple", ""),
        package("mods", "zebra", r#"before = ["apple"]"#),
    ];
    assert_eq!(order(&packages, &LoadOrder::default()), ["zebra", "apple"]);
}

/// An ordering hint about a package nobody installed is not a failure. Warning on it would fire
/// for every mod that mentions a popular optional package.
#[test]
fn an_after_naming_an_absent_package_is_ignored() {
    let packages = [package("mods", "apple", r#"after = ["not_installed"]"#)];
    let resolution = resolve(&packages, &LoadOrder::default());
    assert_eq!(resolution.order, [0]);
    assert!(resolution.problems.is_empty(), "{:?}", resolution.problems);
    assert!(resolution.disabled.is_empty());
}

/// The user wins. The manifest's contradicting edge is dropped, not merged, and the overrule is
/// reported so the resulting order is explainable.
#[test]
fn a_user_constraint_overrules_a_manifest_that_contradicts_it() {
    let packages = [
        package("mods", "apple", r#"after = ["zebra"]"#),
        package("mods", "zebra", ""),
    ];
    let load_order = LoadOrder::parse(
        r#"
        [[constraint]]
        package = "apple"
        before = ["zebra"]
        "#,
    )
    .expect("parses");

    let resolution = resolve(&packages, &load_order);
    assert_eq!(
        resolution
            .order
            .iter()
            .map(|i| packages[*i].manifest.package.id.as_str())
            .collect::<Vec<_>>(),
        ["apple", "zebra"]
    );
    assert_eq!(
        resolution
            .problems
            .iter()
            .map(|p| p.kind)
            .collect::<Vec<_>>(),
        [ename_asset_package::ProblemKind::OverruledConstraint]
    );
}

#[test]
fn an_unsatisfied_requirement_disables_the_package_and_says_why() {
    let packages = [
        package("base", "core", ""),
        package("mods", "apple", r#"requires = ["core >= 2.0"]"#),
    ];
    let resolution = resolve(&packages, &LoadOrder::default());

    assert_eq!(
        resolution
            .order
            .iter()
            .map(|i| packages[*i].manifest.package.id.as_str())
            .collect::<Vec<_>>(),
        ["core"]
    );
    assert_eq!(resolution.disabled.len(), 1);
    assert_eq!(resolution.disabled[0].id, "apple");
    match &resolution.disabled[0].reason {
        DisableReason::Unsatisfied { requirement, found } => {
            assert_eq!(requirement.id, "core");
            assert_eq!(found.as_ref(), Some(&Version::new(1, 0, 0)));
        }
        other => panic!("expected an unsatisfied requirement, got {other:?}"),
    }
}

#[test]
fn a_requirement_on_an_absent_package_disables_it_too() {
    let packages = [package("mods", "apple", r#"requires = ["core"]"#)];
    let resolution = resolve(&packages, &LoadOrder::default());
    assert!(resolution.order.is_empty());
    match &resolution.disabled[0].reason {
        DisableReason::Unsatisfied { found, .. } => assert_eq!(*found, None),
        other => panic!("expected an unsatisfied requirement, got {other:?}"),
    }
}

/// Disabling has to cascade or the game loads a package whose dependency is not there, which is
/// the exact situation `requires` exists to prevent.
#[test]
fn disabling_cascades_to_whatever_required_the_disabled_package() {
    let packages = [
        package("mods", "apple", r#"requires = ["absent"]"#),
        package("mods", "zebra", r#"requires = ["apple"]"#),
    ];
    let resolution = resolve(&packages, &LoadOrder::default());

    assert!(resolution.order.is_empty());
    let zebra = resolution
        .disabled
        .iter()
        .find(|d| d.id == "zebra")
        .expect("zebra is disabled too");
    match &zebra.reason {
        DisableReason::RequirementDisabled { id } => assert_eq!(id, "apple"),
        other => panic!("expected a cascade, got {other:?}"),
    }
}

/// A cycle disables its members and reports them. Loading them in an invented order would be an
/// override nobody asked for, and panicking would let one broken mod take down the game.
#[test]
fn a_cycle_disables_its_members_and_names_them() {
    let packages = [
        package("mods", "apple", r#"after = ["zebra"]"#),
        package("mods", "zebra", r#"after = ["apple"]"#),
    ];
    let resolution = resolve(&packages, &LoadOrder::default());

    assert!(resolution.order.is_empty());
    assert_eq!(resolution.disabled.len(), 2);
    match &resolution.disabled[0].reason {
        DisableReason::Cycle { members } => {
            assert!(members.contains(&"apple".to_owned()));
            assert!(members.contains(&"zebra".to_owned()));
        }
        other => panic!("expected a cycle, got {other:?}"),
    }
    assert_eq!(
        resolution
            .problems
            .iter()
            .map(|p| p.kind)
            .collect::<Vec<_>>(),
        [ename_asset_package::ProblemKind::DependencyCycle]
    );
}

/// A package outside the cycle must survive it.
#[test]
fn a_cycle_costs_only_its_members() {
    let packages = [
        package("base", "core", ""),
        package("mods", "apple", r#"after = ["zebra"]"#),
        package("mods", "zebra", r#"after = ["apple"]"#),
    ];
    let resolution = resolve(&packages, &LoadOrder::default());
    assert_eq!(
        resolution
            .order
            .iter()
            .map(|i| packages[*i].manifest.package.id.as_str())
            .collect::<Vec<_>>(),
        ["core"]
    );
}

/// A package ordered after a cycle cannot load either, but it is not *in* the loop and must not
/// be told it is: the constraints to fix are `a`'s and `b`'s, and `c` has none to look at.
#[test]
fn a_package_behind_a_cycle_is_not_reported_as_one_of_its_members() {
    let packages = [
        package("mods", "a", r#"after = ["b"]"#),
        package("mods", "b", r#"after = ["a"]"#),
        package("mods", "c", r#"after = ["a"]"#),
    ];
    let resolution = resolve(&packages, &LoadOrder::default());

    assert!(resolution.order.is_empty());
    let reason = |id: &str| {
        resolution
            .disabled
            .iter()
            .find(|d| d.id == id)
            .unwrap_or_else(|| panic!("{id} is disabled, got {:?}", resolution.disabled))
            .reason
            .clone()
    };

    for id in ["a", "b"] {
        match reason(id) {
            DisableReason::Cycle { members } => {
                assert_eq!(members, ["a", "b"], "{id} names the loop it is in");
            }
            other => panic!("expected {id} to be a cycle member, got {other:?}"),
        }
    }
    match reason("c") {
        DisableReason::BehindCycle { cycle } => assert_eq!(cycle, ["a", "b"]),
        other => panic!("expected c to be behind the cycle, got {other:?}"),
    }

    let cycles: Vec<&ename_asset_package::Problem> = resolution
        .problems
        .iter()
        .filter(|p| p.kind == ename_asset_package::ProblemKind::DependencyCycle)
        .collect();
    assert_eq!(cycles.len(), 1, "{:?}", resolution.problems);
    assert!(
        cycles[0].detail.ends_with("a, b"),
        "the loop is a and b, and c is not in it: {}",
        cycles[0].detail
    );
}

/// Contest reporting has to say *why* a winner won, so the edges outlive the sort.
#[test]
fn the_resolution_remembers_which_constraint_ordered_two_packages() {
    let packages = [
        package("mods", "apple", r#"after = ["zebra"]"#),
        package("mods", "zebra", ""),
    ];
    let resolution = resolve(&packages, &LoadOrder::default());

    assert!(resolution.edges.direct("zebra", "apple").is_some());
    assert!(resolution.edges.direct("apple", "zebra").is_none());
    assert!(resolution.edges.ordered("zebra", "apple"));
}

/// Transitive ordering counts: if a chain relates two packages, the winner did not come from a
/// tiebreaker and the report must not say it did.
#[test]
fn ordering_is_transitive() {
    let packages = [
        package("mods", "a", ""),
        package("mods", "b", r#"after = ["a"]"#),
        package("mods", "c", r#"after = ["b"]"#),
    ];
    let resolution = resolve(&packages, &LoadOrder::default());
    assert!(resolution.edges.ordered("a", "c"));
    assert!(resolution.edges.direct("a", "c").is_none());
}

/// Attaches assets to a package, so a test can say what it claims.
fn claiming(mut package: Package, aliases: &[&str]) -> Package {
    let root = package.root.clone();
    package.assets = aliases
        .iter()
        .map(|alias| DiscoveredAsset {
            alias: (*alias).to_owned(),
            path: root.join(format!("{}.txt", alias.replace("::", "_"))),
            guid: None,
            origin: AliasOrigin::Derived,
        })
        .collect();
    package
}

/// Runs the resolver and the fold together, the way `scan_packages` does.
fn scan_of(packages: Vec<Package>, load_order: &LoadOrder) -> Scan {
    let resolution = resolve(&packages, load_order);
    Scan {
        packages: resolution
            .order
            .iter()
            .map(|i| packages[*i].clone())
            .collect(),
        disabled: resolution.disabled,
        problems: resolution.problems,
        edges: resolution.edges,
    }
}

/// The line worth acting on: nothing relates these two, so the winner came from a tiebreaker
/// neither author chose.
#[test]
fn two_packages_claiming_one_alias_with_nothing_ordering_them_is_unordered() {
    let scan = scan_of(
        vec![
            claiming(package("mods", "apple", ""), &["core::hull"]),
            claiming(package("mods", "zebra", ""), &["core::hull"]),
        ],
        &LoadOrder::default(),
    );
    let (index, report) = build_index(&scan);

    assert_eq!(report.contests.len(), 1);
    let contest = &report.contests[0];
    assert_eq!(contest.alias, "core::hull");
    assert_eq!(contest.winner.id, "zebra");
    assert_eq!(contest.loser.id, "apple");
    assert!(matches!(
        contest.reason,
        ContestReason::Unordered {
            tiebreak: Tiebreak::DirectoryName { .. }
        }
    ));
    assert!(
        index
            .resolve("core::hull")
            .unwrap()
            .starts_with("mods/zebra")
    );
}

/// When a constraint ordered them, the report says so and names it, because that contest is
/// working as intended and needs no action.
#[test]
fn a_constraint_explains_why_the_winner_won() {
    let scan = scan_of(
        vec![
            claiming(package("base", "core", ""), &["core::hull"]),
            claiming(
                package(
                    "mods",
                    "bigships",
                    r#"after = ["core"]
                    overrides = ["core"]"#,
                ),
                &["core::hull"],
            ),
        ],
        &LoadOrder::default(),
    );
    let (_, report) = build_index(&scan);

    assert_eq!(report.contests.len(), 1);
    assert!(matches!(
        report.contests[0].reason,
        ContestReason::Ordered { direct: true, .. }
    ));
    assert!(
        report.problems.is_empty(),
        "a declared override warns about nothing: {:?}",
        report.problems
    );
}

/// Colliding with a package the manifest never named is the surprise the warning exists for.
#[test]
fn an_undeclared_override_warns() {
    let scan = scan_of(
        vec![
            claiming(package("base", "core", ""), &["core::hull"]),
            claiming(package("mods", "bigships", ""), &["core::hull"]),
        ],
        &LoadOrder::default(),
    );
    let (_, report) = build_index(&scan);

    assert!(
        report
            .problems
            .iter()
            .any(|p| p.kind == ProblemKind::UndeclaredOverride),
        "{:?}",
        report.problems
    );
}

/// The typo catcher: `overrides = ["core"]` with an alias that matches nothing collides with
/// nothing, and the dead entry is the only visible sign the alias was misspelled.
#[test]
fn an_overrides_entry_that_never_collides_warns() {
    let scan = scan_of(
        vec![
            claiming(package("base", "core", ""), &["core::airship"]),
            claiming(
                package("mods", "bigships", r#"overrides = ["core"]"#),
                &["core::airschip"],
            ),
        ],
        &LoadOrder::default(),
    );
    let (_, report) = build_index(&scan);

    assert!(
        report
            .problems
            .iter()
            .any(|p| p.kind == ProblemKind::DeadOverride),
        "{:?}",
        report.problems
    );
}

#[test]
fn removes_takes_an_alias_out_of_the_index() {
    let scan = scan_of(
        vec![
            claiming(package("base", "core", ""), &["core::banana"]),
            package("mods", "nobanana", r#"removes = ["core::banana"]"#),
        ],
        &LoadOrder::default(),
    );
    let (index, report) = build_index(&scan);

    assert_eq!(index.resolve("core::banana"), None);
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

/// A later package may claim an alias an earlier one removed. Removal is a step in load order, not
/// a ban.
#[test]
fn a_later_package_may_reclaim_a_removed_alias() {
    let scan = scan_of(
        vec![
            claiming(package("base", "core", ""), &["core::banana"]),
            package("mods", "a_nobanana", r#"removes = ["core::banana"]"#),
            claiming(package("mods", "b_newbanana", ""), &["core::banana"]),
        ],
        &LoadOrder::default(),
    );
    let (index, _) = build_index(&scan);
    assert!(
        index
            .resolve("core::banana")
            .unwrap()
            .starts_with("mods/b_newbanana")
    );
}

#[test]
fn a_removes_entry_matching_nothing_warns() {
    let scan = scan_of(
        vec![package("mods", "nobanana", r#"removes = ["core::banana"]"#)],
        &LoadOrder::default(),
    );
    let (_, report) = build_index(&scan);

    assert!(
        report
            .problems
            .iter()
            .any(|p| p.kind == ProblemKind::DeadRemoval),
        "{:?}",
        report.problems
    );
}

/// Two loops in one scan are two problems. Telling a member of one that its loop also contains
/// packages it has never heard of sends its author reading manifests that cannot be at fault.
#[test]
fn two_disjoint_cycles_are_reported_separately() {
    let packages = [
        package("mods", "a", r#"after = ["b"]"#),
        package("mods", "b", r#"after = ["a"]"#),
        package("mods", "d", r#"after = ["e"]"#),
        package("mods", "e", r#"after = ["d"]"#),
    ];
    let resolution = resolve(&packages, &LoadOrder::default());

    assert!(resolution.order.is_empty());
    let members = |id: &str| match &resolution
        .disabled
        .iter()
        .find(|d| d.id == id)
        .unwrap_or_else(|| panic!("{id} is disabled, got {:?}", resolution.disabled))
        .reason
    {
        DisableReason::Cycle { members } => members.clone(),
        other => panic!("expected {id} to be a cycle member, got {other:?}"),
    };
    assert_eq!(members("a"), ["a", "b"]);
    assert_eq!(members("b"), ["a", "b"]);
    assert_eq!(members("d"), ["d", "e"]);
    assert_eq!(members("e"), ["d", "e"]);

    let cycles: Vec<&ename_asset_package::Problem> = resolution
        .problems
        .iter()
        .filter(|p| p.kind == ProblemKind::DependencyCycle)
        .collect();
    assert_eq!(cycles.len(), 2, "{:?}", resolution.problems);
}

/// A user constraint can close a loop over manifests that are each individually correct, so the
/// report has to name the constraints rather than the packages: without the source, this reader is
/// sent to three manifests and their own file is the one to edit.
#[test]
fn a_cycle_the_user_closed_names_their_load_order() {
    let packages = [
        package("base", "core", ""),
        package("mods", "a", r#"after = ["core"]"#),
        package("mods", "b", r#"after = ["a"]"#),
    ];
    let load_order = LoadOrder::parse(
        r#"
        [[constraint]]
        package = "core"
        after = ["b"]
        "#,
    )
    .expect("parses");

    let resolution = resolve(&packages, &load_order);
    let cycle = resolution
        .problems
        .iter()
        .find(|p| p.kind == ProblemKind::DependencyCycle)
        .unwrap_or_else(|| panic!("a cycle is reported, got {:?}", resolution.problems));

    assert!(
        cycle
            .detail
            .contains("core after b [the user's load order]"),
        "the constraint that closed the loop is the user's: {}",
        cycle.detail
    );
    assert!(
        cycle.detail.contains("a after core [a's manifest]"),
        "the manifest edges are named with their source too: {}",
        cycle.detail
    );
}
