//! What this project scans, mirroring `example_game_editor_bin`'s own `main.rs`.
//!
//! The asset root, the search paths, and the names that place the user's load order file are game
//! policy, not something `ename_asset_package` or `ename_asset_alias` could derive on their own --
//! the same reason `example_game_editor_bin`'s `main` passes them to `EnginePlugins` instead of
//! the engine defaulting to them. `ename_xtask` is a second target reading the same tree, so it
//! declares the same policy rather than importing it: `example_game_lib` links Bevy. The per-OS
//! layout itself is shared through `ename_application`, so only the names are copied.

use ename_application::{OrganisationName, Paths, ProjectName};
use std::path::PathBuf;

/// Relative to the directory `cargo xtask` is run from, which is the workspace root for every
/// normal invocation. Mirrors `AssetPlugin::file_path`'s default and `example_game_editor_bin`'s
/// own asset root.
pub const ASSET_ROOT: &str = "assets";

/// Mirrors `example_game_editor_bin`'s `main.rs`: `with_content_search_paths(["basegame", "mods"])`.
pub const SEARCH_PATHS: [&str; 2] = ["basegame", "mods"];

/// Mirror `example_game_lib::ORGANISATION_NAME` and `PROJECT_NAME`.
const ORGANISATION_NAME: OrganisationName = OrganisationName::from_static("ename");
const PROJECT_NAME: ProjectName = ProjectName::from_static("example-game");

/// [`SEARCH_PATHS`], owned: `scan_packages` takes `&[String]`.
pub fn search_paths() -> Vec<String> {
    SEARCH_PATHS.iter().map(|s| (*s).to_owned()).collect()
}

/// Where the user's load order file lives: the project's persistent data directory, as the
/// engine's default. `None` when the platform has none.
pub fn load_order_path() -> Option<PathBuf> {
    Some(
        Paths::resolve(&ORGANISATION_NAME, &PROJECT_NAME)
            .persistent_data_path()?
            .join(ename_asset_package::LOAD_ORDER_FILE),
    )
}
