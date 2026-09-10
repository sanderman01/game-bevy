//! `ename_fix` over a mutable copy of the `basic` fixture.

mod support;

use ename_xtask::fix::fix;

#[test]
fn it_creates_a_sidecar_for_a_bulk_asset_and_assigns_the_missing_guid() {
    let dir = support::copy_fixture("basic");
    let root = dir.path();

    let summary = fix(root, &["basegame".to_owned()], None);
    assert_eq!(summary.created, 1, "barrel.png had no sidecar at all");
    assert_eq!(
        summary.guids_assigned, 1,
        "crate.png.alias existed but had no guid"
    );

    let barrel_alias = root.join("basegame/core/props/barrel.png.alias");
    assert!(barrel_alias.exists());
    let text = std::fs::read_to_string(&barrel_alias).unwrap();
    let file: ename_asset_alias::AliasFile = toml::from_str(&text).unwrap();
    assert_eq!(file.alias.as_deref(), Some("core::props/barrel"));
    assert_eq!(file.alias_origin, ename_asset_alias::AliasOrigin::Derived);
    assert!(file.guid.is_some());

    let crate_alias = root.join("basegame/core/props/crate.png.alias");
    let text = std::fs::read_to_string(&crate_alias).unwrap();
    let file: ename_asset_alias::AliasFile = toml::from_str(&text).unwrap();
    assert_eq!(
        file.alias.as_deref(),
        Some("core::props/crate"),
        "the existing alias must not change"
    );
    assert!(file.guid.is_some(), "the missing guid must now be set");
}

#[test]
fn it_never_rewrites_a_sidecar_that_already_has_a_guid() {
    let dir = support::copy_fixture("basic");
    let root = dir.path();
    let ship_alias = root.join("basegame/core/ships/airship.glb.alias");
    let before = std::fs::read_to_string(&ship_alias).unwrap();

    fix(root, &["basegame".to_owned()], None);

    let after = std::fs::read_to_string(&ship_alias).unwrap();
    assert_eq!(
        before, after,
        "a complete sidecar must be left byte-for-byte alone"
    );
}

/// `std::fs::set_permissions` with a read-only mode is the standard, portable-enough-for-this way
/// to force a write to fail on purpose; there is no clean cross-platform equivalent (Windows
/// read-only directories don't block file creation the same way), so this test is Unix-only.
#[test]
#[cfg(unix)]
fn a_write_failure_is_counted_and_does_not_claim_success() {
    use std::os::unix::fs::PermissionsExt;

    let dir = support::copy_fixture("basic");
    let root = dir.path();
    let crate_alias = root.join("basegame/core/props/crate.png.alias");

    // `crate.png.alias` already exists and is missing only its guid, so `fix` takes the
    // rewrite-in-place path for it; stripping the owner's write bit makes that specific write
    // fail without touching the directory permissions `barrel.png`'s from-scratch write needs.
    let mut perms = std::fs::metadata(&crate_alias).unwrap().permissions();
    perms.set_mode(0o444);
    std::fs::set_permissions(&crate_alias, perms.clone()).unwrap();

    let summary = fix(root, &["basegame".to_owned()], None);

    // Restore write permission so the `TempDir` can clean itself up without complaint.
    perms.set_mode(0o644);
    std::fs::set_permissions(&crate_alias, perms).unwrap();

    assert_eq!(
        summary.failed, 1,
        "the unwritable sidecar must be counted as a failure"
    );
    assert_eq!(
        summary.guids_assigned, 0,
        "a failed write must not be counted as if it had succeeded"
    );
    assert_eq!(
        summary.created, 1,
        "the unrelated barrel.png sidecar is unaffected and still gets created"
    );

    let after = std::fs::read_to_string(&crate_alias).unwrap();
    assert!(
        !after.contains("guid"),
        "the write that failed must not have partially landed"
    );
}

#[test]
fn running_fix_twice_is_a_no_op_the_second_time() {
    let dir = support::copy_fixture("basic");
    let root = dir.path();

    let first = fix(root, &["basegame".to_owned()], None);
    assert_eq!(first.created + first.guids_assigned, 2);

    let second = fix(root, &["basegame".to_owned()], None);
    assert_eq!(
        second,
        ename_xtask::fix::FixSummary::default(),
        "everything already has a guid, so the second run does nothing"
    );
}

/// Regression test for the reported bug: a package with no `_alias_rules.toml` anywhere in it
/// made every one of its assets invisible to the scan, so `ename_fix` wrote nothing for them.
/// The package id now stands in for the missing rule.
#[test]
fn it_creates_sidecars_under_the_package_id_when_no_rules_file_covers_the_package() {
    let dir = support::copy_fixture("no_rules");
    let root = dir.path();

    let summary = fix(root, &["basegame".to_owned()], None);
    assert_eq!(
        summary.created, 2,
        "both airship.glb and map.glb had no sidecar and no rule at all"
    );

    let airship = root.join("basegame/core/airship.glb.alias");
    let text = std::fs::read_to_string(&airship).unwrap();
    let file: ename_asset_alias::AliasFile = toml::from_str(&text).unwrap();
    assert_eq!(file.alias.as_deref(), Some("core::airship"));
    assert_eq!(file.alias_origin, ename_asset_alias::AliasOrigin::Derived);
    assert!(file.guid.is_some());

    let map = root.join("basegame/core/map.glb.alias");
    let text = std::fs::read_to_string(&map).unwrap();
    let file: ename_asset_alias::AliasFile = toml::from_str(&text).unwrap();
    assert_eq!(file.alias.as_deref(), Some("core::map"));
}
