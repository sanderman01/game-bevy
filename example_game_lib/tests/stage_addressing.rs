//! Integration tests for the game's side of alias addressing, over a headless `App`.
//!
//! The stage's asset handles are keyed on `alias://`, not on a resolved file path. This does not
//! need the real `assets/` tree, which is a symlink outside the repository and is not present on
//! a fresh clone.

use bevy::{app::TaskPoolPlugin, asset::AssetPlugin, prelude::*};
use ename_asset_content::AssetContentPlugin;

/// Relative to the workspace root, which `BEVY_ASSET_ROOT` pins in `.cargo/config.toml`.
const FIXTURE_ROOT: &str = "example_game_lib/tests/fixtures";

/// The stage must still be keyed on `alias://`, not a resolved path, for the same reason the
/// hand-built-handle version of this test checked it before `open_stage` existed: a
/// resolved-path handle would go stale the moment the active package set changes at runtime.
#[test]
fn stage_is_opened_through_the_alias_source() {
    let mut app = App::new();
    app.add_plugins(
        AssetContentPlugin::default()
            .with_asset_root(FIXTURE_ROOT)
            .with_search_paths(["packages"]),
    )
    .add_plugins(TaskPoolPlugin::default())
    .add_plugins(AssetPlugin {
        file_path: FIXTURE_ROOT.to_owned(),
        ..Default::default()
    })
    .add_plugins(bevy::world_serialization::WorldSerializationPlugin)
    .add_plugins(ename_engine::stage::StagePlugin);

    let entity = ename_engine::stage::open_stage(app.world_mut(), "alias://core::start_stage");

    let handle = app
        .world()
        .get::<DynamicWorldRoot>(entity)
        .expect("open_stage inserts a DynamicWorldRoot");
    let path = handle.0.path().expect("a path-backed handle");
    assert_eq!(
        path.source().as_str(),
        Some("alias"),
        "the stage handle must be keyed on the alias source, got {path}"
    );
    assert_eq!(path.path().to_str(), Some("core::start_stage"));
}
