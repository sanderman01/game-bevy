fn main() {
    let mut bevy_app = bevy::app::App::new();
    engine::app::create_app(&mut bevy_app);
    bevy_app.run();
}
