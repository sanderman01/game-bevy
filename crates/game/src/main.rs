//! Binary target: composes the layers into one `App`.

use bevy::{prelude::*, window::WindowResolution};

fn main() {
    App::new()
        .add_plugins(engine::EnginePlugins::default().with_window(Window {
            title: "My Bevy Game".into(),
            resolution: WindowResolution::new(1280, 720),
            ..default()
        }))
        .add_plugins(modloader::ModLoaderPlugin::default().with_search_paths(["basegame", "mods"]))
        .add_plugins(game::GamePlugins)
        .add_plugins(editor::editor::EditorPluginGroup)
        .run();
}
