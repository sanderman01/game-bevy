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
