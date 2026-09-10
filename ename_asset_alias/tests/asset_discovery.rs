//! What the alias walk finds in a directory tree, over a fake `Vfs`.
//!
//! Folder rules, `.alias` sidecars, and every way the pair can be wrong. No packages and no
//! manifests: `ename_asset_package`'s `package_scan.rs` covers what happens when a caller puts an
//! order on top of this.

mod support;

use ename_asset_alias::{AliasOrigin, AliasScan, DiscoveredAsset, ProblemKind, scan_aliases};
use futures_lite::future::block_on;
use std::path::Path;
use support::FakeVfs;

/// Everything is written under one directory so the tests read the way a package does, and so the
/// walk has a root that is not the whole fake tree.
const ROOT: &str = "base/core";

fn scan(vfs: &FakeVfs) -> AliasScan {
    scan_with_default(vfs, None)
}

fn scan_with_default(vfs: &FakeVfs, default_alias_template: Option<&str>) -> AliasScan {
    block_on(scan_aliases(
        vfs,
        Path::new(ROOT),
        &[],
        default_alias_template,
    ))
}

fn discovered(vfs: &FakeVfs) -> Vec<DiscoveredAsset> {
    scan(vfs).assets
}

fn discovered_with_default(
    vfs: &FakeVfs,
    default_alias_template: Option<&str>,
) -> Vec<DiscoveredAsset> {
    scan_with_default(vfs, default_alias_template).assets
}

fn aliases(assets: &[DiscoveredAsset]) -> Vec<&str> {
    assets.iter().map(|a| a.alias.as_str()).collect()
}

fn kinds(scan: &AliasScan) -> Vec<ProblemKind> {
    scan.problems.iter().map(|p| p.kind).collect()
}

#[test]
fn a_folder_rule_names_every_file_it_includes() {
    let assets = discovered(
        &FakeVfs::new()
            .file("base/core/_alias_rules.toml", r#"alias = "core::{stem}""#)
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
    let assets = discovered(
        &FakeVfs::new()
            .file(
                "base/core/_alias_rules.toml",
                "alias = \"core::{stem}\"\ninclude = [\"*.glb\"]\n",
            )
            .file("base/core/airship.glb", "")
            .file("base/core/README.txt", ""),
    );

    assert_eq!(aliases(&assets), ["core::airship"]);
}

#[test]
fn an_alias_sidecar_overrides_the_derived_name() {
    let assets = discovered(
        &FakeVfs::new()
            .file("base/core/_alias_rules.toml", r#"alias = "core::{stem}""#)
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
    let assets = discovered(
        &FakeVfs::new()
            .file("base/core/_alias_rules.toml", r#"alias = "core::{stem}""#)
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
    let assets = discovered(
        &FakeVfs::new()
            .file(
                "base/core/_alias_rules.toml",
                "alias = \"core::{stem}\"\ninclude = [\"*.glb\"]\n",
            )
            .file("base/core/notes.txt", "")
            .file("base/core/notes.txt.alias", r#"alias = "core::notes""#),
    );

    assert_eq!(aliases(&assets), ["core::notes"]);
}

#[test]
fn a_rule_inherits_into_subdirectories() {
    let assets = discovered(
        &FakeVfs::new()
            .file("base/core/_alias_rules.toml", r#"alias = "core::{path}""#)
            .file("base/core/props/barrel.png", ""),
    );

    assert_eq!(aliases(&assets), ["core::props/barrel"]);
}

/// Replacing rather than merging, so "what does this file get" is answerable by reading one file.
#[test]
fn a_deeper_rule_replaces_the_inherited_one() {
    let assets = discovered(
        &FakeVfs::new()
            .file("base/core/_alias_rules.toml", r#"alias = "core::{path}""#)
            .file(
                "base/core/props/_alias_rules.toml",
                r#"alias = "core::prop_{stem}""#,
            )
            .file("base/core/props/barrel.png", ""),
    );

    assert_eq!(aliases(&assets), ["core::prop_barrel"]);
}

/// One warning per README would bury the warnings that matter.
#[test]
fn a_file_no_rule_covers_is_skipped_silently() {
    let scan = scan(&FakeVfs::new().file("base/core/README.txt", ""));

    assert!(scan.assets.is_empty());
    assert!(scan.problems.is_empty(), "got {:?}", scan.problems);
}

#[test]
fn sidecars_are_never_assets_themselves() {
    let assets = discovered(
        &FakeVfs::new()
            .file("base/core/_alias_rules.toml", r#"alias = "core::{stem}""#)
            .file("base/core/airship.glb", "")
            .file(
                "base/core/airship.glb.alias",
                "guid = \"018f2c00-0000-7000-8000-000000000000\"\n",
            ),
    );

    assert_eq!(
        aliases(&assets),
        ["core::airship"],
        "_alias_rules.toml and the .alias file must not become assets"
    );
}

/// The `.alias` half of a pair that drifted apart. The spec makes this an `xtask content check`
/// failure in phase 4; the scanner is what notices it.
#[test]
fn an_orphan_alias_file_is_reported() {
    let scan = scan(
        &FakeVfs::new()
            .file("base/core/_alias_rules.toml", r#"alias = "core::{stem}""#)
            .file("base/core/deleted.glb.alias", r#"alias = "core::deleted""#),
    );

    assert_eq!(kinds(&scan), [ProblemKind::OrphanAliasFile]);
    assert!(scan.assets.is_empty());
}

/// A guid is only useful if every asset has one, and nothing generates them until phase 4. The
/// scanner reports rather than fails, so the game still runs.
#[test]
fn a_sidecar_without_a_guid_is_reported_but_still_registers() {
    let scan = scan(
        &FakeVfs::new()
            .file("base/core/airship.glb", "")
            .file("base/core/airship.glb.alias", r#"alias = "core::airship""#),
    );

    assert_eq!(kinds(&scan), [ProblemKind::MissingGuid]);
    assert_eq!(aliases(&scan.assets), ["core::airship"]);
}

#[test]
fn a_duplicate_alias_keeps_the_first_and_reports_the_second() {
    let scan = scan(
        &FakeVfs::new()
            .file("base/core/_alias_rules.toml", r#"alias = "core::{stem}""#)
            // Both reduce to the stem `airship`, and the walk sorts, so `.glb` comes first.
            .file("base/core/airship.glb", "")
            .file("base/core/airship.png", ""),
    );

    assert_eq!(kinds(&scan), [ProblemKind::DuplicateAlias]);
    let assets = &scan.assets;
    assert_eq!(aliases(assets), ["core::airship"]);
    assert_eq!(assets[0].path.to_str(), Some("base/core/airship.glb"));
}

#[test]
fn an_unparseable_alias_file_is_reported_and_the_rest_still_load() {
    let scan = scan(
        &FakeVfs::new()
            .file("base/core/_alias_rules.toml", r#"alias = "core::{stem}""#)
            .file("base/core/airship.glb", "")
            .file("base/core/map.glb", "")
            .file("base/core/map.glb.alias", "this is not toml [[["),
    );

    assert_eq!(kinds(&scan), [ProblemKind::UnparseableAliasFile]);
    assert_eq!(
        aliases(&scan.assets),
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
            .file("base/core/_alias_rules.toml", r#"alias = "core::{path}""#)
            .file("base/core/props/_alias_rules.toml", "alias = [[[")
            .file("base/core/props/barrel.png", ""),
    );

    assert_eq!(kinds(&scan), [ProblemKind::UnparseableRules]);
    assert_eq!(aliases(&scan.assets), ["core::props/barrel"]);
}

#[test]
fn an_unreadable_directory_is_reported() {
    let scan = scan(
        &FakeVfs::new()
            .file("base/core/_alias_rules.toml", r#"alias = "core::{stem}""#)
            .file("base/core/airship.glb", "")
            .file("base/core/locked/secret.glb", "")
            .unreadable_dir("base/core/locked"),
    );

    assert_eq!(kinds(&scan), [ProblemKind::UnreadableDirectory]);
    assert_eq!(aliases(&scan.assets), ["core::airship"]);
}

/// Pins the order the walk produces within one directory: alphabetical, and files before it
/// descends into subdirectories. (Whether that order is stable *across runs* -- i.e. does not
/// come from the platform's own iteration order -- is a claim about `StdVfs`'s sort, which this
/// fake, backed by a `BTreeSet`, cannot exercise either way; see
/// `package_scan.rs::assets_come_back_in_the_same_order_every_time`.)
#[test]
fn assets_are_ordered_alphabetically_with_files_before_subdirectories() {
    let vfs = FakeVfs::new()
        .file("base/core/_alias_rules.toml", r#"alias = "core::{path}""#)
        .file("base/core/zebra.glb", "")
        .file("base/core/apple.glb", "")
        .file("base/core/props/barrel.glb", "");

    assert_eq!(
        aliases(&discovered(&vfs)).join(","),
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
        .file("base/core/_alias_rules.toml", r#"alias = "core::{path}""#)
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
        aliases(&scan.assets),
        ["core::shallow"],
        "the shallow asset must still register; one runaway subtree must not cost the rest"
    );
}

/// The walk skips `_alias_rules.toml`, `*.alias` and `*.meta` on its own. Anything else a
/// caller's own format owns has to be named, or a permissive rule sweeps it up as content --
/// `ename_asset_package` passes `manifest.toml` for exactly this reason.
#[test]
fn a_caller_can_name_files_the_walk_must_not_treat_as_assets() {
    let vfs = FakeVfs::new()
        .file("base/core/_alias_rules.toml", r#"alias = "core::{stem}""#)
        .file("base/core/manifest.toml", "")
        .file("base/core/airship.glb", "");

    assert_eq!(
        aliases(&discovered(&vfs)),
        ["core::airship", "core::manifest"],
        "nothing is ignored by default beyond the alias layer's own files"
    );

    let scan = block_on(scan_aliases(
        &vfs,
        Path::new(ROOT),
        &["manifest.toml"],
        None,
    ));
    assert_eq!(aliases(&scan.assets), ["core::airship"]);
}

/// An alias `AssetPath` would misread can never resolve, so it is reported here rather than
/// silently dropped by whoever builds the index. This crate owns the alias type, so this is the
/// first place the check can happen at all.
#[test]
fn an_alias_the_validator_rejects_is_reported_and_costs_only_itself() {
    let scan = scan(
        &FakeVfs::new()
            .file("base/core/_alias_rules.toml", r#"alias = "core::{stem}""#)
            .file("base/core/airship.glb", "")
            .file("base/core/bad.glb", "")
            .file("base/core/bad.glb.alias", r#"alias = "core::bad#Scene0""#),
    );

    // Only `InvalidAlias`: an alias that can never resolve is not also worth a guid warning.
    assert_eq!(kinds(&scan), [ProblemKind::InvalidAlias]);
    assert_eq!(aliases(&scan.assets), ["core::airship"]);
}

/// macOS and Windows preserve whatever case a file was saved in, so a rule saved as
/// `_Alias_Rules.toml` has to be read as a rule. Left case-sensitive it was worse than ignored: it
/// became an asset named after itself, and the folder it was meant to name derived nothing.
#[test]
fn a_rules_file_saved_in_another_case_is_still_a_rule() {
    let scan = scan(
        &FakeVfs::new()
            .file("base/core/_Alias_Rules.toml", r#"alias = "core::{stem}""#)
            .file("base/core/airship.glb", ""),
    );

    assert_eq!(aliases(&scan.assets), ["core::airship"]);
    assert!(scan.problems.is_empty(), "got {:?}", scan.problems);
}

/// Only a case-sensitive filesystem can hold both at once, and then the canonical spelling wins,
/// so the directory behaves the way it does everywhere else.
#[test]
fn the_canonical_spelling_wins_where_a_filesystem_holds_several() {
    let assets = discovered(
        &FakeVfs::new()
            // `_Alias_Rules.toml` sorts first: uppercase `R` is below lowercase `r`. Picking the
            // first match rather than the canonical one would take the wrong template here.
            .file("base/core/_Alias_Rules.toml", r#"alias = "wrong::{stem}""#)
            .file("base/core/_alias_rules.toml", r#"alias = "core::{stem}""#)
            .file("base/core/airship.glb", ""),
    );

    assert_eq!(aliases(&assets), ["core::airship"]);
}

/// The same reasoning as the rule file: a caller's own file saved in another case must not become
/// content just because the disk remembered the shift key.
#[test]
fn an_ignored_file_name_is_matched_case_insensitively() {
    let vfs = FakeVfs::new()
        .file("base/core/_alias_rules.toml", r#"alias = "core::{stem}""#)
        .file("base/core/Manifest.toml", "")
        .file("base/core/airship.glb", "");

    let scan = block_on(scan_aliases(
        &vfs,
        Path::new(ROOT),
        &["manifest.toml"],
        None,
    ));
    assert_eq!(aliases(&scan.assets), ["core::airship"]);
}

/// A directory with no rule at all still gets every file named, when the caller supplies a
/// default template -- the package id standing in for the rule nobody wrote.
#[test]
fn a_default_template_covers_a_file_with_no_rule_and_no_sidecar() {
    let assets = discovered_with_default(
        &FakeVfs::new()
            .file("base/core/airship.glb", "")
            .file("base/core/map.glb", ""),
        Some("core::{stem}"),
    );

    assert_eq!(aliases(&assets), ["core::airship", "core::map"]);
    assert_eq!(
        assets[0].origin,
        AliasOrigin::Derived,
        "the default derived it, so tooling may rewrite it"
    );
}

/// The default behaves exactly like a rule written at the root: a real `_alias_rules.toml`
/// anywhere in the tree still replaces it outright.
#[test]
fn an_explicit_rule_still_replaces_the_default() {
    let assets = discovered_with_default(
        &FakeVfs::new()
            .file("base/core/_alias_rules.toml", r#"alias = "other::{stem}""#)
            .file("base/core/airship.glb", ""),
        Some("core::{stem}"),
    );

    assert_eq!(aliases(&assets), ["other::airship"]);
}

/// The default is the fallback for a subdirectory too, not only the walk's own root.
#[test]
fn the_default_inherits_into_a_subdirectory_with_no_rule_of_its_own() {
    let assets = discovered_with_default(
        &FakeVfs::new().file("base/core/props/barrel.glb", ""),
        Some("core::{stem}"),
    );

    assert_eq!(aliases(&assets), ["core::barrel"]);
}

/// An `.alias` sidecar still wins over the default, the same as it wins over a real rule.
#[test]
fn a_sidecar_alias_still_wins_over_the_default() {
    let assets = discovered_with_default(
        &FakeVfs::new().file("base/core/ship_final_v2.glb", "").file(
            "base/core/ship_final_v2.glb.alias",
            "guid = \"018f2c00-0000-7000-8000-000000000000\"\nalias = \"core::airship\"\n",
        ),
        Some("core::{stem}"),
    );

    assert_eq!(aliases(&assets), ["core::airship"]);
    assert_eq!(assets[0].origin, AliasOrigin::Authored);
}
