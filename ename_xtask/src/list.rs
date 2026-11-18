//! `ename_content_list` -- dumps the resolved alias index.

use ename_asset_package::build_index;
use std::path::Path;

/// Runs the scan and prints every alias, sorted, with what it resolves to.
pub fn list(asset_root: &Path, search_paths: &[String], load_order_path: Option<&Path>) {
    let (scan, _) = crate::scan::scan(asset_root, search_paths, load_order_path);
    let (index, _) = build_index(&scan);
    let mut entries: Vec<(String, &Path)> = index.iter().collect();
    entries.sort();
    for (alias, path) in entries {
        println!("{alias:32} {}", path.display());
    }
}
