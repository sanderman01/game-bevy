//! Integration tests for package discovery, over a real `AssetReader`.
//!
//! No `App` here. `scan_packages` is a plain `async fn` over an `ErasedAssetReader`, so the test
//! constructs the reader directly and blocks on it. `FileAssetReader` runs its io on the
//! `blocking` crate's pool, not on a Bevy task pool, so there is nothing to schedule.

use bevy::{
    asset::io::{AssetSource, ErasedAssetReader},
    tasks::block_on,
};
use ename_asset_package::{Package, Version, scan_packages};

/// Relative to the workspace root, which `BEVY_ASSET_ROOT` pins in `.cargo/config.toml`.
const FIXTURE_ROOT: &str = "crates/ename_asset_package/tests/fixtures";

fn scan(search_paths: &[&str]) -> Vec<Package> {
    let mut make_reader = AssetSource::get_default_reader(FIXTURE_ROOT.to_owned());
    let reader: Box<dyn ErasedAssetReader> = make_reader();
    let paths: Vec<String> = search_paths.iter().map(|p| (*p).to_owned()).collect();
    block_on(scan_packages(reader.as_ref(), &paths))
}

fn ids(packages: &[Package]) -> Vec<&str> {
    packages
        .iter()
        .map(|p| p.manifest.package.id.as_str())
        .collect()
}

/// A directory holding a `manifest.toml` is a package. The scan does not recurse past that.
#[test]
fn finds_every_package_under_every_search_path() {
    let packages = scan(&["base", "mods"]);
    assert_eq!(ids(&packages), ["core", "loud", "trimmed"]);
}

/// Ordering is what decides which package wins an override, so it is asserted rather than assumed.
/// Search paths come in the order the caller gave them, which is how a target says "mods load
/// after the base game" once, in the binary.
#[test]
fn search_paths_load_in_the_order_they_were_given() {
    assert_eq!(ids(&scan(&["base", "mods"])), ["core", "loud", "trimmed"]);
    assert_eq!(ids(&scan(&["mods", "base"])), ["loud", "trimmed", "core"]);
}

/// Within one search path the order is by directory name, never by whatever the filesystem hands
/// back. Without the sort in `read_packages_in` this passes on one machine and fails on another.
#[test]
fn packages_in_one_search_path_are_ordered_by_directory_name() {
    assert_eq!(ids(&scan(&["mods"])), ["loud", "trimmed"]);
}

#[test]
fn a_package_carries_its_own_root_directory() {
    let packages = scan(&["base", "mods"]);
    assert_eq!(packages[0].root.to_str(), Some("base/core"));
    assert_eq!(packages[1].root.to_str(), Some("mods/loud"));
}

#[test]
fn manifest_fields_round_trip_from_toml() {
    let packages = scan(&["mods"]);
    let loud = &packages[0];
    assert_eq!(loud.manifest.package.title, "Loud test package");
    assert_eq!(loud.manifest.package.version, Version::new(1, 2, 3));
    assert!(loud.manifest.assets.replace.is_some());
}

/// One malformed mod must cost that mod and nothing else. `broken` sorts before `loud` by name, so
/// a skip that silently truncated the rest of the directory would still look right in the ordering
/// test above; this one names the package that must be absent.
#[test]
fn an_unparseable_manifest_is_skipped_and_the_rest_still_load() {
    let packages = scan(&["mods"]);
    assert!(
        !packages.iter().any(|p| p.root.ends_with("broken")),
        "the broken package must not appear"
    );
    assert_eq!(
        packages.len(),
        2,
        "and it must not take its neighbours with it"
    );
}

/// A missing search path must not take the scan down, and must not stop the packages that were
/// found from registering.
#[test]
fn a_missing_search_path_is_survivable() {
    assert_eq!(
        ids(&scan(&["base", "does_not_exist", "mods"])),
        ["core", "loud", "trimmed"]
    );
}

#[test]
fn no_search_paths_finds_nothing() {
    assert!(scan(&[]).is_empty());
}
