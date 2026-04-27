use bevy::prelude::*;

pub fn create_app(app: &mut bevy::app::App) {
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "My Bevy Game".into(),
                    resolution: bevy::window::WindowResolution::new(1280, 720),
                    ..default()
                }),
                ..default()
            })
            .disable::<bevy::transform::TransformPlugin>(),
    )
    .add_plugins(big_space::plugin::BigSpaceDefaultPlugins)
    .add_systems(Startup, crate::scene::new_simple_scene)
    .add_systems(
        Startup,
        crate::scene::load_model.after(crate::scene::new_simple_scene),
    )
    .add_systems(
        PostUpdate,
        crate::camera_controller::custom_big_space_camera_inputs
            .before(big_space::camera::camera_controller),
    )
    .add_plugins(editor::editor::EditorPluginGroup);
}
