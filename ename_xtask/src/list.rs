//! `ename_content_list` -- dumps the resolved alias index.

use ename_asset_package::build_index;
use std::path::Path;

/// Runs the scan and prints every alias, sorted, with what it resolves to.
pub fn list(asset_root: &Path, search_paths: &[String], load_order_path: Option<&Path>) {
    let scan = crate::scan::scan(asset_root, search_paths, load_order_path);
    let (index, _) = build_index(&scan);
    let mut entries: Vec<(&str, &Path)> = index.iter().collect();
    entries.sort_by_key(|(alias, _)| *alias);
    for (alias, path) in entries {
        println!("{alias:32} {}", path.display());
    }
}
