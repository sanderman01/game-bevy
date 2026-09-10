//! Integration tests for the game's side of alias addressing, over a headless `App`.
//!
//! Two properties, both of which phase 1 changed:
//!
//! 1. `GameState` leaves `Loading` without waiting on the content layer.
//! 2. The scene's asset handles are keyed on `alias://`, not on a resolved file path.
//!
//! Neither needs the real `assets/` tree, which is a symlink outside the repository and is not
//! present on a fresh clone.

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::{AssetApp, AssetPlugin},
    prelude::*,
    state::app::StatesPlugin,
};
use ename_asset_alias::ALIAS_SOURCE;
use ename_asset_content::AssetContentPlugin;
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
        GameState::Scene,
        "reaches Scene without any content plugin in the App"
    );
}

/// The scene must address assets through `alias://`. If a handle were keyed on a resolved path,
/// changing the active package set at runtime would leave every live handle pointing at the old
/// file, which is the property the whole design exists to protect.
#[test]
fn scene_handles_are_keyed_on_the_alias_source() {
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
    // The asset type only, not `ScenePlugin`: the test reads the handle's path and never loads.
    .init_asset::<WorldAsset>();

    let server = app.world().resource::<AssetServer>().clone();

    // Exactly what `load_models` builds -- `WorldAssetRoot` is a `Handle<WorldAsset>` -- without
    // dragging `ScenePlugin`'s renderer requirements in.
    let handle = server.load::<WorldAsset>(
        GltfAssetLabel::Scene(0).from_asset(format!("{ALIAS_SOURCE}://core::airship")),
    );

    let path = handle.path().expect("a path-backed handle");
    assert_eq!(
        path.source().as_str(),
        Some("alias"),
        "the handle must be keyed on the alias source, got {path}"
    );
    assert_eq!(path.path().to_str(), Some("core::airship"));
    assert_eq!(path.label(), Some("Scene0"));
}
