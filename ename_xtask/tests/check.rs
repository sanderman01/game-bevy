//! `ename_check` over real fixture trees.

use ename_asset_package::ContentKinds;
use ename_xtask::check::check;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn no_kinds() -> ContentKinds {
    ContentKinds::default()
}

fn map_kind() -> ContentKinds {
    let mut kinds = ContentKinds::default();
    kinds.insert("map", "map.ron").unwrap();
    kinds
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A `core` package with one map.
fn content_tree() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "basegame/core/package-info.toml",
        "[package]\nid = \"core\"\nversion = \"1.0.0\"\nauthors = []\ntitle = \"Core\"\ndescription = \"\"\n",
    );
    write(root.path(), "basegame/core/island.map.ron", "()");
    root
}

#[test]
fn a_missing_guid_fails_the_check() {
    let root = fixture("basic");
    assert!(!check(&root, &["basegame".to_owned()], None, &no_kinds()));
}

#[test]
fn a_contested_alias_fails_the_check_even_when_declared() {
    let root = fixture("contest");
    assert!(!check(
        &root,
        &["basegame".to_owned(), "mods".to_owned()],
        None,
        &no_kinds()
    ));
}

#[test]
fn a_clean_tree_passes() {
    let root = fixture("clean");
    assert!(check(&root, &["basegame".to_owned()], None, &no_kinds()));
}

#[test]
fn a_clean_tree_with_content_passes() {
    let root = content_tree();
    assert!(check(
        root.path(),
        &["basegame".to_owned()],
        None,
        &map_kind()
    ));
}

/// The alias rule cuts the stem at the first dot, so `island.png` claims `core::island` as well.
#[test]
fn a_duplicate_alias_of_a_content_file_fails_the_check() {
    let root = content_tree();
    write(root.path(), "basegame/core/island.png", "");
    assert!(!check(
        root.path(),
        &["basegame".to_owned()],
        None,
        &map_kind()
    ));
}
