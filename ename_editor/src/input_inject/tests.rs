use std::time::Duration;

use bevy::{
    ecs::message::Messages,
    input::{
        ButtonState, InputPlugin,
        keyboard::{Key, KeyboardFocusLost, KeyboardInput},
    },
    picking::{
        DefaultPickingPlugins,
        pointer::{PointerAction, PointerButton, PointerId, PointerInput, PointerLocation},
    },
    window::{PrimaryWindow, WindowEvent, WindowResolution},
};
use ename_engine::{
    command::{
        Caller, CommandPlugin, CommandScope, CommandWorldExt, JobId, JobStatus, Jobs, Reply,
    },
    debug_switch::{DebugSwitchAppExt, DebugSwitchPlugin, DebugSwitchSpec, KeyChord},
};
use serde_json::{Value, json};

use super::*;
use crate::input_commands::InputCommandsPlugin;

#[derive(Resource, Reflect, Default)]
struct Toggle {
    on: bool,
}

/// A headless editor-less app: input, picking, a 400x300 logical window at scale factor 2.
fn app() -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        InputPlugin,
        CommandPlugin,
        DebugSwitchPlugin,
        DefaultPickingPlugins,
        InputInjectPlugin,
        InputCommandsPlugin,
    ))
    // `WindowPlugin`'s messages, which `bevy_winit` would write.
    .add_message::<WindowEvent>()
    .add_message::<CursorMoved>()
    .init_resource::<Toggle>()
    .add_field_debug_switch::<Toggle>(
        DebugSwitchSpec::new("test", "test.toggle", "Toggle.")
            .key(KeyChord::new(KeyCode::F4).ctrl()),
        "on",
    );
    app.world_mut().spawn((
        Window {
            resolution: WindowResolution::new(800, 600).with_scale_factor_override(2.0),
            ..default()
        },
        PrimaryWindow,
    ));
    app.finish();
    app.update();
    app
}

fn window(app: &mut App) -> Entity {
    app.world_mut()
        .query_filtered::<Entity, With<PrimaryWindow>>()
        .single(app.world())
        .unwrap()
}

fn send(app: &mut App, params: Value) -> Result<JobId, CommandError> {
    let caller = Caller {
        scope: CommandScope::Development,
        cwd: None,
    };
    match app.world_mut().call_command("input.send", params, caller)? {
        Reply::Job(job) => Ok(job),
        Reply::Done(value) => panic!("expected a job, got {value}"),
    }
}

fn status(app: &App, job: &JobId) -> JobStatus {
    app.world()
        .resource::<Jobs>()
        .status(job, CommandScope::Development)
        .unwrap()
}

/// Updates until the job ends, at most 100 frames.
fn run(app: &mut App, job: &JobId) -> JobStatus {
    for _ in 0..100 {
        app.update();
        let status = status(app, job);
        if status.state != "running" {
            return status;
        }
    }
    panic!("job {job} did not end");
}

fn keys(app: &App) -> &ButtonInput<KeyCode> {
    app.world().resource::<ButtonInput<KeyCode>>()
}

fn real_key(app: &mut App, key_code: KeyCode, state: ButtonState) {
    let window = window(app);
    app.world_mut().write_message(KeyboardInput {
        key_code,
        logical_key: keymap::logical_key(key_code),
        state,
        text: None,
        repeat: false,
        window,
    });
}

fn mouse_location(app: &mut App) -> Option<Vec2> {
    app.world_mut()
        .query::<(&PointerId, &PointerLocation)>()
        .iter(app.world())
        .find(|(id, _)| **id == PointerId::Mouse)
        .and_then(|(_, location)| location.location.as_ref().map(|l| l.position))
}

/// Pointer inputs written since `cursor` last read.
fn pointer_inputs(
    app: &App,
    cursor: &mut bevy::ecs::message::MessageCursor<PointerInput>,
) -> Vec<PointerInput> {
    cursor
        .read(app.world().resource::<Messages<PointerInput>>())
        .cloned()
        .collect()
}

#[test]
fn key_step_presses_for_exactly_one_frame_and_releases() {
    let mut app = app();
    // The trailing `key_down` is still held when the script ends, so the end releases it.
    let job = send(
        &mut app,
        json!({"steps": [{"action": "key", "key": "F5"}, {"action": "key_down", "key": "KeyW"}]}),
    )
    .unwrap();
    let mut just_pressed = 0;
    for _ in 0..5 {
        app.update();
        just_pressed += usize::from(keys(&app).just_pressed(KeyCode::F5));
    }
    assert_eq!(just_pressed, 1);
    assert!(!keys(&app).pressed(KeyCode::F5));
    assert!(!keys(&app).pressed(KeyCode::KeyW));
    let status = status(&app, &job);
    assert_eq!(status.state, "succeeded");
    assert_eq!(status.result.unwrap()["already_held"], json!([]));
}

#[test]
fn ctrl_f4_reaches_a_debug_switch() {
    let mut app = app();
    let job = send(
        &mut app,
        json!({"steps": [{"action": "key", "key": "F4", "modifiers": ["ctrl"]}]}),
    )
    .unwrap();
    run(&mut app, &job);
    assert!(app.world().resource::<Toggle>().on);
    assert!(!keys(&app).pressed(KeyCode::ControlLeft));
}

#[test]
fn cancelled_hold_releases() {
    let mut app = app();
    let job = send(
        &mut app,
        json!({"steps": [
            {"action": "move", "x": 100, "y": 50},
            {"action": "key_down", "key": "KeyW"},
            {"action": "button_down"},
            {"action": "wait", "seconds": 100}
        ]}),
    )
    .unwrap();
    for _ in 0..4 {
        app.update();
    }
    let buttons = |app: &App| app.world().resource::<ButtonInput<MouseButton>>().clone();
    assert!(keys(&app).pressed(KeyCode::KeyW));
    assert!(buttons(&app).pressed(MouseButton::Left));
    app.world_mut()
        .cancel_job(&job, CommandScope::Development)
        .unwrap();
    let status = run(&mut app, &job);
    assert_eq!(status.state, "cancelled");
    app.update();
    assert!(!keys(&app).pressed(KeyCode::KeyW));
    assert!(!buttons(&app).pressed(MouseButton::Left));
    assert!(!app.world().resource::<InputInjector>().is_busy());
}

#[test]
fn click_reaches_picking_in_the_frame_it_is_written() {
    let mut app = app();
    let mut cursor = app
        .world()
        .resource::<Messages<PointerInput>>()
        .get_cursor();
    send(
        &mut app,
        json!({"steps": [{"action": "click", "x": 100, "y": 50}]}),
    )
    .unwrap();
    app.update();
    assert_eq!(mouse_location(&mut app), Some(Vec2::new(50.0, 25.0)));
    pointer_inputs(&app, &mut cursor);

    // The user's mouse moves between the move and the press; the press stays on target.
    let window = window(&mut app);
    app.world_mut()
        .write_message(WindowEvent::CursorMoved(CursorMoved {
            window,
            position: Vec2::new(300.0, 200.0),
            delta: None,
        }));
    app.update();
    let press = pointer_inputs(&app, &mut cursor)
        .into_iter()
        .find(|input| matches!(input.action, PointerAction::Press(PointerButton::Primary)))
        .expect("the press reached picking in the frame it was written");
    assert_eq!(press.location.position, Vec2::new(50.0, 25.0));
}

#[test]
fn real_held_modifier_survives_a_key_step_and_cleanup() {
    let mut app = app();
    real_key(&mut app, KeyCode::ShiftLeft, ButtonState::Pressed);
    app.update();
    let job = send(
        &mut app,
        json!({"steps": [{"action": "key", "key": "KeyA", "modifiers": ["shift"]}]}),
    )
    .unwrap();
    let status = run(&mut app, &job);
    app.update();
    assert!(keys(&app).pressed(KeyCode::ShiftLeft));
    assert_eq!(status.result.unwrap()["already_held"], json!(["ShiftLeft"]));
}

#[test]
fn a_stalled_script_times_out_releases_and_frees_the_injector() {
    let mut app = app();
    let job = send(
        &mut app,
        json!({"steps": [{"action": "key_down", "key": "KeyW"}, {"action": "wait", "seconds": 100}],
               "timeout_seconds": 1}),
    )
    .unwrap();
    app.update();
    assert!(keys(&app).pressed(KeyCode::KeyW));
    assert_eq!(
        send(&mut app, json!({"steps": [{"action": "leave"}]}))
            .unwrap_err()
            .kind,
        "input_busy"
    );
    app.world_mut().resource_mut::<InputClock>().offset = Duration::from_secs(2);
    let status = run(&mut app, &job);
    assert_eq!(status.state, "failed");
    assert_eq!(status.error.unwrap().kind, "script_timeout");
    app.update();
    assert!(!keys(&app).pressed(KeyCode::KeyW));
    let next = send(&mut app, json!({"steps": [{"action": "leave"}]})).unwrap();
    assert_eq!(run(&mut app, &next).state, "succeeded");
}

#[test]
fn focus_loss_stops_the_script_and_releases_logical_keys_too() {
    let mut app = app();
    let job = send(
        &mut app,
        json!({"steps": [{"action": "key_down", "key": "KeyW"}, {"action": "wait", "seconds": 100}]}),
    )
    .unwrap();
    app.update();
    let w = Key::Character("w".into());
    assert!(
        app.world()
            .resource::<ButtonInput<Key>>()
            .pressed(w.clone())
    );
    app.world_mut().write_message(KeyboardFocusLost);
    let status = run(&mut app, &job);
    assert_eq!(status.error.unwrap().kind, "focus_lost");
    app.update();
    assert!(!app.world().resource::<ButtonInput<Key>>().pressed(w));
    assert!(!keys(&app).pressed(KeyCode::KeyW));
}

#[test]
fn validation_errors() {
    let mut app = app();
    for (params, kind) in [
        (
            json!({"steps": [{"action": "key", "key": "F13x"}]}),
            "unknown_key",
        ),
        (
            json!({"steps": [{"action": "move", "x": 800, "y": 10}]}),
            "out_of_source",
        ),
        (
            json!({"steps": [{"action": "move", "x": 1, "y": 1, "in": "editor_camera"}]}),
            "camera_inactive",
        ),
        (json!({"steps": [{"action": "wait"}]}), "step_failed"),
        (json!({"steps": []}), "no_steps"),
        // Huge values would panic in `Duration` or queue billions of frames.
        (
            json!({"steps": [{"action": "leave"}], "timeout_seconds": 1e20}),
            "invalid_params",
        ),
        (
            json!({"steps": [{"action": "wait", "seconds": 1e20}]}),
            "step_failed",
        ),
        (
            json!({"steps": [{"action": "drag", "from": [1, 1], "to": [2, 2], "frames": 4_000_000_000u32}]}),
            "step_failed",
        ),
    ] {
        assert_eq!(
            send(&mut app, params.clone()).unwrap_err().kind,
            kind,
            "{params}"
        );
    }
    assert!(!app.world().resource::<InputInjector>().is_busy());
}

#[test]
fn text_types_characters() {
    let mut app = app();
    let mut cursor = app
        .world()
        .resource::<Messages<KeyboardInput>>()
        .get_cursor();
    let job = send(
        &mut app,
        json!({"steps": [{"action": "text", "text": "Hi!"}]}),
    )
    .unwrap();
    run(&mut app, &job);
    let typed: Vec<_> = cursor
        .read(app.world().resource::<Messages<KeyboardInput>>())
        .filter(|input| input.state == ButtonState::Pressed)
        .map(|input| (input.key_code, input.text.clone().unwrap().to_string()))
        .collect();
    assert_eq!(
        typed,
        [
            (KeyCode::KeyH, "H".to_owned()),
            (KeyCode::KeyI, "i".to_owned()),
            (
                KeyCode::Unidentified(bevy::input::keyboard::NativeKeyCode::Unidentified),
                "!".to_owned()
            ),
        ]
    );
}
