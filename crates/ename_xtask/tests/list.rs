//! `ename_list` over real fixture trees.

use ename_xtask::list::list;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn listing_a_clean_tree_does_not_panic() {
    list(&fixture("clean"), &["basegame".to_owned()], None);
}

#[test]
fn listing_a_tree_with_problems_still_lists_what_it_found() {
    // `basic` has a MissingGuid problem but the asset is still in the index -- `ename_check`
    // fails it, `ename_list` still shows it, and that difference is the whole reason the two are
    // separate commands.
    list(&fixture("basic"), &["basegame".to_owned()], None);
}
