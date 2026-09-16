//! Binary target: composes the layers into one `App`.
//!
//! Which crates link into this binary is a property of the target, not of the engine. A shipping
//! build genuinely does not contain the editor rather than containing it behind a runtime check.

use bevy::{prelude::*, window::WindowResolution};

fn main() {
    let mut app = App::new();

    let mut engine = ename_engine::EnginePlugins::default()
        .with_window(Window {
            title: "My Bevy Game".into(),
            resolution: WindowResolution::new(1360, 710),
            ..default()
        })
        .with_content_search_paths(["basegame", "mods", "examples"]);
    if let Some(path) = load_order_path() {
        engine = engine.with_load_order_file(path);
    }

    app.add_plugins(engine)
        .add_plugins(example_game_lib::GamePlugins);

    app.add_plugins(ename_editor::EditorPlugins);
    app.add_plugins(ename_remote::RemotePlugins);

    app.run();
}

/// Where the user's load order file lives.
///
/// The binary owns this because a config directory is platform policy, and the asset crates read a
/// path they are given so a test can point them at a fixture instead. `None` on a platform with no
/// config directory, which simply means no user constraints.
fn load_order_path() -> Option<std::path::PathBuf> {
    Some(
        dirs::config_dir()?
            .join("ename")
            .join(ename_engine::LOAD_ORDER_FILE),
    )
}
