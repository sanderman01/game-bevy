use bevy::input::{ButtonState, InputPlugin, keyboard::Key, keyboard::KeyboardInput};
use serde_json::{Value, json};

use super::*;
use crate::command::{Caller, CommandError, CommandPlugin, CommandScope, CommandWorldExt, Reply};

#[derive(Resource, Reflect, Default, Clone, Copy)]
struct Overlays {
    physics: bool,
    nested: Nested,
}

#[derive(Reflect, Default, Clone, Copy)]
struct Nested {
    grids: bool,
}

#[derive(Resource, Reflect, Default, Clone, Copy, Debug, PartialEq, Eq)]
enum Quality {
    #[default]
    Low,
    FieldOnly,
    Ultra,
}

#[derive(Resource, Default)]
struct Flag(Option<bool>);

fn app() -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        InputPlugin,
        CommandPlugin,
        DebugSwitchPlugin,
    ))
    .init_resource::<Overlays>()
    .init_resource::<Quality>();
    app
}

fn finish(mut app: App) -> App {
    app.finish();
    app.update();
    app
}

fn press(app: &mut App, chord: &[KeyCode]) {
    for &key in chord {
        write_key(app, key, ButtonState::Pressed);
    }
    app.update();
    for &key in chord.iter().rev() {
        write_key(app, key, ButtonState::Released);
    }
    app.update();
}

fn write_key(app: &mut App, key_code: KeyCode, state: ButtonState) {
    app.world_mut().write_message(KeyboardInput {
        key_code,
        logical_key: Key::Unidentified(bevy::input::keyboard::NativeKey::Unidentified),
        state,
        text: None,
        repeat: false,
        window: Entity::PLACEHOLDER,
    });
}

fn physics(app: &mut App) -> &mut App {
    app.add_field_debug_switch::<Overlays>(
        DebugSwitchSpec::new("overlay", "overlay.physics", "Physics.")
            .key(KeyChord::new(KeyCode::F5)),
        "physics",
    )
}

fn quality(app: &mut App) -> &mut App {
    app.add_resource_debug_switch::<Quality>(
        DebugSwitchSpec::new("quality", "quality.level", "Quality.")
            .key(KeyChord::new(KeyCode::F4).ctrl()),
    )
}

#[test]
fn key_toggles_bool_and_cycles_enum_with_wrap() {
    let mut app = app();
    physics(&mut app);
    quality(&mut app);
    let mut app = finish(app);

    press(&mut app, &[KeyCode::F5]);
    assert!(app.world().resource::<Overlays>().physics);
    press(&mut app, &[KeyCode::F5]);
    assert!(!app.world().resource::<Overlays>().physics);

    for expected in [Quality::FieldOnly, Quality::Ultra, Quality::Low] {
        press(&mut app, &[KeyCode::ControlLeft, KeyCode::F4]);
        assert_eq!(*app.world().resource::<Quality>(), expected);
    }
}

#[test]
fn modifiers_match_exactly() {
    let mut app = app();
    physics(&mut app);
    quality(&mut app);
    let mut app = finish(app);

    press(&mut app, &[KeyCode::ShiftLeft, KeyCode::F5]);
    assert!(!app.world().resource::<Overlays>().physics);
    press(&mut app, &[KeyCode::F4]);
    assert_eq!(*app.world().resource::<Quality>(), Quality::Low);
    press(
        &mut app,
        &[KeyCode::ControlRight, KeyCode::ShiftLeft, KeyCode::F4],
    );
    assert_eq!(*app.world().resource::<Quality>(), Quality::Low);
    press(&mut app, &[KeyCode::SuperLeft, KeyCode::F5]);
    assert!(!app.world().resource::<Overlays>().physics);
}

#[test]
fn per_target_chords() {
    let mut app = app();
    app.add_field_debug_switch::<Overlays>(
        DebugSwitchSpec::new("overlay", "overlay.physics", "Physics.")
            .key(KeyChord::new(KeyCode::F5))
            .editor_key(None),
        "physics",
    )
    .add_field_debug_switch::<Overlays>(
        DebugSwitchSpec::new("overlay", "overlay.grids", "Grids.")
            .key(KeyChord::new(KeyCode::F5))
            .release_key(Some(KeyChord::new(KeyCode::F6))),
        "nested.grids",
    );
    let mut app = finish(app);

    // Release: F5 is physics, F6 is grids.
    press(&mut app, &[KeyCode::F5]);
    assert!(app.world().resource::<Overlays>().physics);
    press(&mut app, &[KeyCode::F6]);
    assert!(app.world().resource::<Overlays>().nested.grids);

    // Editor: physics has no key, F5 is grids, F6 is nothing.
    *app.world_mut().resource_mut::<DebugSwitchTarget>() = DebugSwitchTarget::Editor;
    press(&mut app, &[KeyCode::F5]);
    let overlays = *app.world().resource::<Overlays>();
    assert!(overlays.physics);
    assert!(!overlays.nested.grids);
    press(&mut app, &[KeyCode::F6]);
    assert!(!app.world().resource::<Overlays>().nested.grids);
}

fn flag_get(world: &World) -> Option<bool> {
    world.resource::<Flag>().0
}

fn flag_set(world: &mut World, value: bool) {
    world.resource_mut::<Flag>().0 = Some(value);
}

#[test]
fn callback_switch_records_launch_value_when_it_first_applies() {
    let mut app = app();
    app.init_resource::<Flag>().add_bool_debug_switch(
        DebugSwitchSpec::new("camera", "camera.flag", "Flag."),
        flag_get,
        flag_set,
    );
    let mut app = finish(app);
    let err = call(
        &mut app,
        "debug.set",
        json!({"values": {"camera.flag": true}}),
    )
    .unwrap_err();
    assert_eq!(err.kind, "switch_unavailable");
    let err = call(&mut app, "debug.reset", json!({"names": ["camera.flag"]})).unwrap_err();
    assert_eq!(err.kind, "switch_unavailable");

    app.world_mut().resource_mut::<Flag>().0 = Some(false);
    app.update();
    call(
        &mut app,
        "debug.set",
        json!({"values": {"camera.flag": true}}),
    )
    .unwrap();
    call(&mut app, "debug.reset", json!({})).unwrap();
    assert_eq!(app.world().resource::<Flag>().0, Some(false));
}

fn call(app: &mut App, name: &str, params: Value) -> Result<Value, CommandError> {
    let caller = Caller {
        scope: CommandScope::Safe,
        cwd: None,
    };
    match app.world_mut().call_command(name, params, caller)? {
        Reply::Done(value) => Ok(value),
        Reply::Job(job) => panic!("unexpected job {job}"),
    }
}

fn commands_app() -> App {
    let mut app = app();
    physics(&mut app);
    quality(&mut app);
    app.add_field_debug_switch::<Overlays>(
        DebugSwitchSpec::new("overlay", "overlay.grids", "Grids."),
        "nested.grids",
    );
    finish(app)
}

#[test]
fn list_filters_by_category() {
    let mut app = commands_app();
    let all = call(&mut app, "debug.list", json!({})).unwrap();
    assert_eq!(all["target"], "release");
    assert_eq!(all["categories"], json!(["overlay", "quality"]));
    assert_eq!(all["switches"].as_array().unwrap().len(), 3);

    let overlay = call(&mut app, "debug.list", json!({"category": "overlay"})).unwrap();
    let names: Vec<_> = overlay["switches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|switch| switch["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["overlay.grids", "overlay.physics"]);
    assert_eq!(overlay["categories"], json!(["overlay", "quality"]));

    let quality = &all["switches"][2];
    assert_eq!(quality["value"], "low");
    assert_eq!(quality["allowed"], json!(["low", "field_only", "ultra"]));
    assert_eq!(
        quality["keys"],
        json!({"editor": "Ctrl+F4", "release": "Ctrl+F4"})
    );
    assert_eq!(quality["launch_value"], "low");

    let err = call(&mut app, "debug.list", json!({"category": "nope"})).unwrap_err();
    assert_eq!(err.kind, "unknown_category");
}

#[test]
fn set_is_atomic() {
    let mut app = commands_app();
    let err = call(
        &mut app,
        "debug.set",
        json!({"values": {"overlay.physics": true, "quality.level": "nope"}}),
    )
    .unwrap_err();
    assert_eq!(err.kind, "invalid_value");
    assert!(!app.world().resource::<Overlays>().physics);
    let err = call(
        &mut app,
        "debug.set",
        json!({"values": {"overlay.physics": "true"}}),
    )
    .unwrap_err();
    assert_eq!(err.kind, "invalid_value");
    let err = call(&mut app, "debug.set", json!({"values": {}})).unwrap_err();
    assert_eq!(err.kind, "no_values");
}

#[test]
fn set_reports_only_changes_and_reset_restores() {
    let mut app = commands_app();
    let set = call(
        &mut app,
        "debug.set",
        json!({"values": {"overlay.physics": false, "quality.level": "field_only"}}),
    )
    .unwrap();
    assert_eq!(
        set["changed"],
        json!([{"name": "quality.level", "from": "low", "to": "field_only"}])
    );
    assert_eq!(*app.world().resource::<Quality>(), Quality::FieldOnly);
    assert_eq!(set["switches"].as_array().unwrap().len(), 2);

    let reset = call(&mut app, "debug.reset", json!({})).unwrap();
    assert_eq!(reset["changed"].as_array().unwrap().len(), 1);
    assert_eq!(*app.world().resource::<Quality>(), Quality::Low);
}
