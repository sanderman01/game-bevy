//! Binary target: composes the layers into one `App`.
//!
//! Which crates link into this binary is a property of the target, not of the engine. A shipping
//! build genuinely does not contain the editor rather than containing it behind a runtime check.

use bevy::{prelude::*, window::WindowResolution};

fn main() {
    let mut app = App::new();

    app.add_plugins(ename_engine::EnginePlugins::default().with_window(Window {
        title: "My Bevy Game".into(),
        resolution: WindowResolution::new(1280, 720),
        ..default()
    }))
    .add_plugins(ename_content::ContentPlugin::default().with_search_paths(["basegame", "mods"]))
    .add_plugins(ename_game::GamePlugins);

    #[cfg(feature = "editor")]
    app.add_plugins(ename_editor::EditorPlugins)
        .add_plugins(ename_game_editor::GameEditorPlugins);

    app.run();
}
