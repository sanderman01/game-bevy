//! `ename_check` over real fixture trees.

use ename_xtask::check::check;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn a_missing_guid_fails_the_check() {
    let root = fixture("basic");
    assert!(!check(&root, &["basegame".to_owned()], None));
}

#[test]
fn a_contested_alias_fails_the_check_even_when_declared() {
    let root = fixture("contest");
    assert!(!check(
        &root,
        &["basegame".to_owned(), "mods".to_owned()],
        None
    ));
}

#[test]
fn a_clean_tree_passes() {
    let root = fixture("clean");
    assert!(check(&root, &["basegame".to_owned()], None));
}
