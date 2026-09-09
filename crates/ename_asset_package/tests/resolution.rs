//! The resolver, over hand-built packages.
//!
//! No filesystem and no `App`: `resolve` is a pure function from a baseline-ordered package list
//! and a user load order to a load order, so every rule in it can be stated as a value in and a
//! value out. The tests that prove the same rules survive a real directory tree are in
//! `package_scan.rs`.

use ename_asset_package::{DisableReason, LoadOrder, Manifest, Package, Version, resolve};
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
/// path first, directory name second. Adding constraints must only ever move what they name.
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
