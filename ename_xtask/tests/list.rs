//! `ename_content_list` over real fixture trees.

use ename_asset_alias::StdVfs;
use ename_asset_package::{LoadOrder, build_index, scan_packages};
use ename_xtask::list::list;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn listing_a_clean_tree_resolves_correct_aliases() {
    let root = fixture("clean");
    let search_paths = vec!["basegame".to_owned()];

    // Scan using the public API, same as the internal scan::scan does.
    let vfs = StdVfs::new(&root);
    let load_order = LoadOrder::default();
    let scan = futures_lite::future::block_on(scan_packages(&vfs, &search_paths, &load_order));
    let (index, _) = build_index(&scan);

    // Assert the index contains the expected aliases from the clean fixture.
    assert!(
        index.resolve("core::airship").is_some(),
        "clean fixture should contain core::airship alias"
    );
    assert_eq!(
        index
            .resolve("core::airship")
            .map(|p| p.to_string_lossy().to_string()),
        Some("basegame/core/ships/airship.glb".to_string()),
        "core::airship should resolve to the airship asset"
    );

    // Also verify list() doesn't panic over the clean tree.
    list(&root, &search_paths, None);
}

#[test]
fn listing_a_tree_with_problems_includes_those_assets_in_the_index() {
    // `basic` has a MissingGuid problem on crate.png.alias but the asset is still in the index --
    // `ename_check` fails it, `ename_content_list` still shows it, and that difference is the whole reason
    // the two are separate commands.
    let root = fixture("basic");
    let search_paths = vec!["basegame".to_owned()];

    // Scan using the public API, same as the internal scan::scan does.
    let vfs = StdVfs::new(&root);
    let load_order = LoadOrder::default();
    let scan = futures_lite::future::block_on(scan_packages(&vfs, &search_paths, &load_order));
    let (index, _) = build_index(&scan);

    // Assert the index contains all expected aliases, including those with problems.
    assert!(
        index.resolve("core::airship").is_some(),
        "basic fixture should contain core::airship alias"
    );
    assert_eq!(
        index
            .resolve("core::airship")
            .map(|p| p.to_string_lossy().to_string()),
        Some("basegame/core/ships/airship.glb".to_string()),
        "core::airship should resolve correctly"
    );

    assert!(
        index.resolve("core::props/barrel").is_some(),
        "basic fixture should contain core::props/barrel alias from _alias_rules.toml"
    );
    assert_eq!(
        index
            .resolve("core::props/barrel")
            .map(|p| p.to_string_lossy().to_string()),
        Some("basegame/core/props/barrel.png".to_string()),
        "core::props/barrel should resolve to barrel.png"
    );

    assert!(
        index.resolve("core::props/crate").is_some(),
        "basic fixture should contain core::props/crate alias even though it has a MissingGuid problem"
    );
    assert_eq!(
        index
            .resolve("core::props/crate")
            .map(|p| p.to_string_lossy().to_string()),
        Some("basegame/core/props/crate.png".to_string()),
        "core::props/crate should resolve to crate.png despite the MissingGuid problem"
    );

    // Also verify list() doesn't panic over the problematic tree.
    list(&root, &search_paths, None);
}
