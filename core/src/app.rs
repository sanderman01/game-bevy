use bevy::prelude::*;

pub fn create_app(app: &mut bevy::app::App) {
    app
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "My Bevy Game".into(),
                resolution: bevy::window::WindowResolution::new(1280, 720),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(bevy::camera_controller::free_camera::FreeCameraPlugin)
        .add_systems(Startup, crate::scene::new_simple_scene);
}