//! Integration tests for package discovery, over a real filesystem.
//!
//! No `App` and no Bevy: `scan_packages` takes a `&dyn Vfs`, and `StdVfs` is the `std::fs`
//! implementation a command line tool would use. `futures_lite::future::block_on` drives it,
//! because `StdVfs`'s io is blocking and there is nothing to schedule.

use ename_asset_package::{Package, StdVfs, Version, scan_packages};
use futures_lite::future::block_on;

/// Relative to this crate's manifest directory: Cargo runs a test binary with that as the
/// working directory, not the workspace root. The old test relied on Bevy's `FileAssetReader`
/// resolving through `BEVY_ASSET_ROOT` instead, which is workspace-root-relative; `StdVfs` has
/// no such indirection, so the fixture path changes to match where the process actually runs.
const FIXTURE_ROOT: &str = "tests/fixtures";

fn scan(search_paths: &[&str]) -> Vec<Package> {
    let vfs = StdVfs::new(FIXTURE_ROOT);
    let paths: Vec<String> = search_paths.iter().map(|p| (*p).to_owned()).collect();
    block_on(scan_packages(&vfs, &paths)).packages
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
    assert_eq!(loud.manifest.package.authors, ["Test"]);
}

/// The fake `Vfs` covers the discovery rules exhaustively. This proves the same walk works over a
/// real filesystem, which is the other implementation of the trait.
#[test]
fn a_package_carries_the_assets_its_folder_rule_names() {
    let packages = scan(&["base"]);
    let assets = &packages[0].assets;
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].alias, "core::greeting");
    assert_eq!(assets[0].path.to_str(), Some("base/core/greeting.txt"));
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

/// The running game reads directories through Bevy's `AssetReader`, whose `is_directory` follows
/// symlinks. `StdVfs` must agree, or a symlinked package directory is found by the game and
/// silently skipped by a command line tool -- this repo's own `assets/` is exactly that shape.
///
/// The symlink is created and removed here rather than committed under `tests/fixtures/`: a
/// checked-in symlink is a portability trap.
#[cfg(unix)]
#[test]
fn a_symlinked_package_directory_is_found_like_a_real_one() {
    let root = std::env::temp_dir().join(format!(
        "ename_asset_package_symlink_test_{}",
        std::process::id()
    ));

    /// Removes the temp tree on the way out, including when an assertion below panics.
    struct RemoveOnDrop(std::path::PathBuf);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = RemoveOnDrop(root.clone());
    let _ = std::fs::remove_dir_all(&root);

    let search_dir = root.join("search");
    std::fs::create_dir_all(&search_dir).expect("create the search directory");

    let real_package = std::fs::canonicalize(std::path::Path::new(FIXTURE_ROOT).join("base/core"))
        .expect("canonicalize the real fixture package");
    std::os::unix::fs::symlink(&real_package, search_dir.join("core")).expect("create the symlink");

    let vfs = StdVfs::new(&root);
    let packages = block_on(scan_packages(&vfs, &["search".to_owned()])).packages;

    assert_eq!(ids(&packages), ["core"]);
}
