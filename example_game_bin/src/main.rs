//! Binary target for a game: composes the layers into one `App`.
//!
//! Which crates link into this binary is a property of the target, not of the engine. A shipping
//! build genuinely does not contain the editor rather than containing it behind a runtime check.

use bevy::{prelude::*, window::WindowResolution};

fn main() {
    let mut app = App::new();

    let engine = ename_engine::EnginePlugins::new(
        example_game_lib::ORGANISATION_NAME,
        example_game_lib::PROJECT_NAME,
    )
    .with_window(Window {
        title: "My Bevy Game".into(),
        resolution: WindowResolution::new(1360, 710),
        ..default()
    })
    .with_content_search_paths(["basegame", "mods", "examples"]);

    app.add_plugins(engine)
        .add_plugins(example_game_lib::GamePlugins);

    app.run();
}
