//! What this project scans, mirroring `ename`'s own `main.rs`.
//!
//! The asset root, the search paths, and where the user's load order file lives are game policy,
//! not something `ename_asset_package` or `ename_asset_alias` could derive on their own -- the
//! same reason `ename`'s `main` passes them to `AssetContentPlugin` instead of the plugin
//! defaulting to them. `ename_xtask` is a second target reading the same tree, so it declares the
//! same policy rather than importing it: both crates it depends on must stay free of Bevy and of
//! platform path lookups, so there is nowhere lower to put two constants both targets could share
//! without adding a third crate for them.

use std::path::PathBuf;

/// Relative to the directory `cargo xtask` is run from, which is the workspace root for every
/// normal invocation. Mirrors `AssetPlugin::file_path`'s default and `ename`'s own asset root.
pub const ASSET_ROOT: &str = "assets";

/// Mirrors `ename`'s `main.rs`: `with_content_search_paths(["basegame", "mods"])`.
pub const SEARCH_PATHS: [&str; 2] = ["basegame", "mods"];

/// [`SEARCH_PATHS`], owned: `scan_packages` takes `&[String]`.
pub fn search_paths() -> Vec<String> {
    SEARCH_PATHS.iter().map(|s| (*s).to_owned()).collect()
}

/// Where the user's load order file lives, if the platform has a config directory at all.
/// Mirrors `ename`'s own `load_order_path`.
pub fn load_order_path() -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("ename")
            .join(ename_asset_package::LOAD_ORDER_FILE),
    )
}
