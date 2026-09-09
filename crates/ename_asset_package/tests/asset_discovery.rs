//! What the scanner finds inside a package, over a fake `Vfs`.
//!
//! `package_scan.rs` covers finding the packages themselves against a real tree. This file covers
//! the walk inside one: folder rules, `.alias` sidecars, and every way the pair can be wrong.

mod support;

use ename_asset_package::{AliasOrigin, DiscoveredAsset, ProblemKind, Scan, scan_packages};
use futures_lite::future::block_on;
use support::FakeVfs;

/// The smallest thing that counts as a package, so each test writes only what it is about.
const MANIFEST: &str = r#"
[package]
id = "core"
version = "1.0.0"
authors = []
title = "Core"
description = ""
"#;

fn scan(vfs: &FakeVfs) -> Scan {
    block_on(scan_packages(vfs, &["base".to_owned()]))
}

fn package(vfs: &FakeVfs) -> Vec<DiscoveredAsset> {
    let scan = scan(vfs);
    assert_eq!(scan.packages.len(), 1, "expected exactly one package");
    scan.packages.into_iter().next().unwrap().assets
}

fn aliases(assets: &[DiscoveredAsset]) -> Vec<&str> {
    assets.iter().map(|a| a.alias.as_str()).collect()
}

fn kinds(scan: &Scan) -> Vec<ProblemKind> {
    scan.problems.iter().map(|p| p.kind).collect()
}

#[test]
fn a_folder_rule_names_every_file_it_includes() {
    let assets = package(
        &FakeVfs::new()
            .file("base/core/manifest.toml", MANIFEST)
            .file("base/core/_rules.toml", r#"alias = "core::{stem}""#)
            .file("base/core/airship.glb", "")
            .file("base/core/map.glb", ""),
    );

    assert_eq!(aliases(&assets), ["core::airship", "core::map"]);
    assert_eq!(assets[0].path.to_str(), Some("base/core/airship.glb"));
    assert_eq!(
        assets[0].origin,
        AliasOrigin::Derived,
        "a rule derived it, so tooling may rewrite it"
    );
}

#[test]
fn a_file_the_rule_excludes_is_not_discovered() {
    let assets = package(
        &FakeVfs::new()
            .file("base/core/manifest.toml", MANIFEST)
            .file(
                "base/core/_rules.toml",
                "alias = \"core::{stem}\"\ninclude = [\"*.glb\"]\n",
            )
            .file("base/core/airship.glb", "")
            .file("base/core/README.txt", ""),
    );

    assert_eq!(aliases(&assets), ["core::airship"]);
}

#[test]
fn an_alias_sidecar_overrides_the_derived_name() {
    let assets = package(
        &FakeVfs::new()
            .file("base/core/manifest.toml", MANIFEST)
            .file("base/core/_rules.toml", r#"alias = "core::{stem}""#)
            .file("base/core/ship_final_v2.glb", "")
            .file(
                "base/core/ship_final_v2.glb.alias",
                "guid = \"018f2c00-0000-7000-8000-000000000000\"\nalias = \"core::airship\"\n",
            ),
    );

    assert_eq!(aliases(&assets), ["core::airship"]);
    assert_eq!(
        assets[0].origin,
        AliasOrigin::Authored,
        "a human typed it, so tooling must never rewrite it"
    );
    assert!(assets[0].guid.is_some());
}

#[test]
fn a_sidecar_can_exclude_a_file_the_rule_covers() {
    let assets = package(
        &FakeVfs::new()
            .file("base/core/manifest.toml", MANIFEST)
            .file("base/core/_rules.toml", r#"alias = "core::{stem}""#)
            .file("base/core/airship.glb", "")
            .file("base/core/wip.glb", "")
            .file("base/core/wip.glb.alias", "include = false\n"),
    );

    assert_eq!(aliases(&assets), ["core::airship"]);
}

/// Somebody wrote that file on purpose. Making them also add `include = true` to a folder whose
/// patterns happen not to match would be a rule nobody could guess.
#[test]
fn a_sidecar_alias_is_included_even_where_the_rule_does_not_match() {
    let assets = package(
        &FakeVfs::new()
            .file("base/core/manifest.toml", MANIFEST)
            .file(
                "base/core/_rules.toml",
                "alias = \"core::{stem}\"\ninclude = [\"*.glb\"]\n",
            )
            .file("base/core/notes.txt", "")
            .file("base/core/notes.txt.alias", r#"alias = "core::notes""#),
    );

    assert_eq!(aliases(&assets), ["core::notes"]);
}

#[test]
fn a_rule_inherits_into_subdirectories() {
    let assets = package(
        &FakeVfs::new()
            .file("base/core/manifest.toml", MANIFEST)
            .file("base/core/_rules.toml", r#"alias = "core::{path}""#)
            .file("base/core/props/barrel.png", ""),
    );

    assert_eq!(aliases(&assets), ["core::props/barrel"]);
}

/// Replacing rather than merging, so "what does this file get" is answerable by reading one file.
#[test]
fn a_deeper_rule_replaces_the_inherited_one() {
    let assets = package(
        &FakeVfs::new()
            .file("base/core/manifest.toml", MANIFEST)
            .file("base/core/_rules.toml", r#"alias = "core::{path}""#)
            .file(
                "base/core/props/_rules.toml",
                r#"alias = "core::prop_{stem}""#,
            )
            .file("base/core/props/barrel.png", ""),
    );

    assert_eq!(aliases(&assets), ["core::prop_barrel"]);
}

/// One warning per README would bury the warnings that matter.
#[test]
fn a_file_no_rule_covers_is_skipped_silently() {
    let scan = scan(
        &FakeVfs::new()
            .file("base/core/manifest.toml", MANIFEST)
            .file("base/core/README.txt", ""),
    );

    assert!(scan.packages[0].assets.is_empty());
    assert!(scan.problems.is_empty(), "got {:?}", scan.problems);
}

#[test]
fn sidecars_are_never_assets_themselves() {
    let assets = package(
        &FakeVfs::new()
            .file("base/core/manifest.toml", MANIFEST)
            .file("base/core/_rules.toml", r#"alias = "core::{stem}""#)
            .file("base/core/airship.glb", "")
            .file(
                "base/core/airship.glb.alias",
                "guid = \"018f2c00-0000-7000-8000-000000000000\"\n",
            ),
    );

    assert_eq!(
        aliases(&assets),
        ["core::airship"],
        "manifest.toml, _rules.toml and the .alias file must not become assets"
    );
}

/// The `.alias` half of a pair that drifted apart. The spec makes this an `xtask content check`
/// failure in phase 4; the scanner is what notices it.
#[test]
fn an_orphan_alias_file_is_reported() {
    let scan = scan(
        &FakeVfs::new()
            .file("base/core/manifest.toml", MANIFEST)
            .file("base/core/_rules.toml", r#"alias = "core::{stem}""#)
            .file("base/core/deleted.glb.alias", r#"alias = "core::deleted""#),
    );

    assert_eq!(kinds(&scan), [ProblemKind::OrphanAliasFile]);
    assert!(scan.packages[0].assets.is_empty());
}

/// A guid is only useful if every asset has one, and nothing generates them until phase 4. The
/// scanner reports rather than fails, so the game still runs.
#[test]
fn a_sidecar_without_a_guid_is_reported_but_still_registers() {
    let scan = scan(
        &FakeVfs::new()
            .file("base/core/manifest.toml", MANIFEST)
            .file("base/core/airship.glb", "")
            .file("base/core/airship.glb.alias", r#"alias = "core::airship""#),
    );

    assert_eq!(kinds(&scan), [ProblemKind::MissingGuid]);
    assert_eq!(aliases(&scan.packages[0].assets), ["core::airship"]);
}

#[test]
fn a_duplicate_alias_keeps_the_first_and_reports_the_second() {
    let scan = scan(
        &FakeVfs::new()
            .file("base/core/manifest.toml", MANIFEST)
            .file("base/core/_rules.toml", r#"alias = "core::{stem}""#)
            // Both reduce to the stem `airship`, and the walk sorts, so `.glb` comes first.
            .file("base/core/airship.glb", "")
            .file("base/core/airship.png", ""),
    );

    assert_eq!(kinds(&scan), [ProblemKind::DuplicateAlias]);
    let assets = &scan.packages[0].assets;
    assert_eq!(aliases(assets), ["core::airship"]);
    assert_eq!(assets[0].path.to_str(), Some("base/core/airship.glb"));
}

#[test]
fn an_unparseable_alias_file_is_reported_and_the_rest_still_load() {
    let scan = scan(
        &FakeVfs::new()
            .file("base/core/manifest.toml", MANIFEST)
            .file("base/core/_rules.toml", r#"alias = "core::{stem}""#)
            .file("base/core/airship.glb", "")
            .file("base/core/map.glb", "")
            .file("base/core/map.glb.alias", "this is not toml [[["),
    );

    assert_eq!(kinds(&scan), [ProblemKind::UnparseableAliasFile]);
    assert_eq!(
        aliases(&scan.packages[0].assets),
        ["core::airship", "core::map"],
        "the broken sidecar costs its own overrides and nothing else"
    );
}

/// The directory had the inherited rule before somebody added the broken file, so that is what it
/// keeps.
#[test]
fn an_unparseable_rules_file_leaves_the_inherited_rule_in_force() {
    let scan = scan(
        &FakeVfs::new()
            .file("base/core/manifest.toml", MANIFEST)
            .file("base/core/_rules.toml", r#"alias = "core::{path}""#)
            .file("base/core/props/_rules.toml", "alias = [[[")
            .file("base/core/props/barrel.png", ""),
    );

    assert_eq!(kinds(&scan), [ProblemKind::UnparseableRules]);
    assert_eq!(aliases(&scan.packages[0].assets), ["core::props/barrel"]);
}

#[test]
fn an_unreadable_directory_is_reported() {
    let scan = scan(
        &FakeVfs::new()
            .file("base/core/manifest.toml", MANIFEST)
            .file("base/core/_rules.toml", r#"alias = "core::{stem}""#)
            .file("base/core/airship.glb", "")
            .file("base/core/locked/secret.glb", "")
            .unreadable_dir("base/core/locked"),
    );

    assert_eq!(kinds(&scan), [ProblemKind::UnreadableDirectory]);
    assert_eq!(aliases(&scan.packages[0].assets), ["core::airship"]);
}

/// Pins the order the walk produces within one directory: alphabetical, and files before it
/// descends into subdirectories. (Whether that order is stable *across runs* -- i.e. does not
/// come from the platform's own iteration order -- is a claim about `StdVfs`'s sort, which this
/// fake, backed by a `BTreeSet`, cannot exercise either way; see
/// `package_scan.rs::assets_come_back_in_the_same_order_every_time`.)
#[test]
fn assets_are_ordered_alphabetically_with_files_before_subdirectories() {
    let vfs = FakeVfs::new()
        .file("base/core/manifest.toml", MANIFEST)
        .file("base/core/_rules.toml", r#"alias = "core::{path}""#)
        .file("base/core/zebra.glb", "")
        .file("base/core/apple.glb", "")
        .file("base/core/props/barrel.glb", "");

    assert_eq!(
        aliases(&package(&vfs)).join(","),
        "core::apple,core::zebra,core::props/barrel"
    );
}

/// A symlink cycle cannot be built with `FakeVfs` -- it has no notion of a symlink at all -- so
/// this builds the equivalent failure: a directory chain deeper than any real asset tree, which is
/// exactly what an unbounded recursion through a cycle would look like from the walk's side.
/// Without the depth cap this either overflows the stack or runs forever.
#[test]
fn a_directory_chain_deeper_than_the_cap_is_reported_and_does_not_cost_the_rest_of_the_package() {
    let mut vfs = FakeVfs::new()
        .file("base/core/manifest.toml", MANIFEST)
        .file("base/core/_rules.toml", r#"alias = "core::{path}""#)
        .file("base/core/shallow.glb", "");

    let mut deep_path = "base/core".to_owned();
    for level in 0..100 {
        deep_path.push_str(&format!("/lvl{level}"));
    }
    deep_path.push_str("/too_deep.glb");
    vfs = vfs.file(&deep_path, "");

    let scan = scan(&vfs);

    assert_eq!(
        kinds(&scan),
        [ProblemKind::DirectoryTooDeep],
        "got {:?}",
        scan.problems
    );
    assert_eq!(
        aliases(&scan.packages[0].assets),
        ["core::shallow"],
        "the shallow asset must still register; one runaway subtree must not cost the package"
    );
}
