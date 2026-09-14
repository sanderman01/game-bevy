//! Integration tests for the game's side of alias addressing, over a headless `App`.
//!
//! Two properties, both of which phase 1 changed:
//!
//! 1. `GameState` leaves `Loading` without waiting on the content layer.
//! 2. The stage's asset handles are keyed on `alias://`, not on a resolved file path.
//!
//! Neither needs the real `assets/` tree, which is a symlink outside the repository and is not
//! present on a fresh clone.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::AssetPlugin,
    prelude::*,
    state::app::StatesPlugin,
};
use ename_asset_content::AssetContentPlugin;
use ename_engine::stage::StageAppExt;
use ename_game::{GameState, GameStatePlugin};

/// Relative to the workspace root, which `BEVY_ASSET_ROOT` pins in `.cargo/config.toml`.
const FIXTURE_ROOT: &str = "ename_game/tests/fixtures";

/// `GameState` used to wait for `LoaderState::AssetsRegistered`, which meant the content layer's
/// internal bookkeeping was part of the game's lifecycle. It should now advance on its own, with
/// no content plugin present at all.
#[test]
fn game_state_leaves_loading_without_the_content_layer() {
    let mut app = App::new();
    app.add_plugins(TaskPoolPlugin::default())
        .add_plugins(StatesPlugin)
        .add_plugins(GameStatePlugin);

    assert_eq!(
        *app.world().resource::<State<GameState>>().get(),
        GameState::Loading,
        "starts in Loading"
    );

    // One update runs Startup and queues the transition; the next applies it.
    app.update();
    app.update();

    assert_eq!(
        *app.world().resource::<State<GameState>>().get(),
        GameState::Stage,
        "reaches Stage without any content plugin in the App"
    );
}

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
    .add_plugins(ename_engine::stage::StagePlugin)
    .register_stage_format(ename_engine::stage::DynamicWorldFormat);

    let entity = ename_engine::stage::open_stage(
        app.world_mut(),
        &format!("{}://core::start_stage", ename_asset_alias::ALIAS_SOURCE),
    );

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
