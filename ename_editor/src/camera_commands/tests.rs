use std::time::Duration;

use bevy::{reflect::GetPath, time::TimeUpdateStrategy};
use ename_engine::{
    bigspace::{BigSpace, BigSpaceCameraController, BigSpaceDefaultPlugins, BigSpacePlugin, Grid},
    camera::ground::{CameraGroundGuard, GroundAnswer, GroundProbeAppExt},
    command::{Caller, CommandPlugin, CommandScope, CommandWorldExt, JobId, JobStatus, Reply},
    wait::WaitPlugin,
};
use serde_json::{Value, json};

use super::*;
use crate::camera::{EditorCameraPlugin, orbit_focus};

/// Ground that ignores the point's height, like streamed terrain.
#[derive(Resource, Default)]
struct Terrain {
    height: Option<f64>,
    not_loaded: bool,
}

fn terrain_probe(In(_): In<DVec3>, terrain: Res<Terrain>) -> GroundAnswer {
    match (terrain.not_loaded, terrain.height) {
        (true, _) => GroundAnswer::NotLoaded,
        (false, Some(height)) => GroundAnswer::Surface(height),
        (false, None) => GroundAnswer::NoGround,
    }
}

pub(crate) fn base_app() -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        bevy::input::InputPlugin,
        BigSpaceDefaultPlugins,
        BigSpacePlugin,
        EditorCameraPlugin {
            settings: default(),
        },
        CommandPlugin,
        WaitPlugin,
        CameraCommandsPlugin,
    ))
    .insert_resource(UiState::new())
    .init_resource::<Terrain>()
    .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
        1.0 / 60.0,
    )));
    app
}

/// The editor camera attached to a root grid, as in the editor with a stage loaded.
pub(crate) fn finish(mut app: App) -> App {
    app.world_mut().spawn((
        BigSpace::default(),
        Grid::new(100.0, 0.0),
        Transform::default(),
        Name::new("stage"),
    ));
    app.update();
    app.update();
    app
}

fn app() -> App {
    finish(base_app())
}

fn app_with_terrain() -> App {
    let mut app = base_app();
    app.add_ground_probe(terrain_probe);
    finish(app)
}

pub(crate) fn editor_camera(app: &mut App) -> Entity {
    app.world_mut()
        .query_filtered::<Entity, With<EditorCamera>>()
        .single(app.world())
        .unwrap()
}

pub(crate) fn call(app: &mut App, name: &str, params: Value) -> Result<Reply, CommandError> {
    app.world_mut().call_command(
        name,
        params,
        Caller {
            scope: CommandScope::Development,
            cwd: None,
        },
    )
}

fn start(app: &mut App, params: Value) -> JobId {
    match call(app, "camera.look_at", params).unwrap() {
        Reply::Job(job) => job,
        Reply::Done(value) => panic!("not a job: {value}"),
    }
}

fn status(app: &App, job: &JobId) -> JobStatus {
    app.world()
        .resource::<Jobs>()
        .status(job, CommandScope::Development)
        .unwrap()
}

/// Runs frames until the job ends; panics after `limit`.
pub(crate) fn run(app: &mut App, job: &JobId, limit: usize) -> (JobStatus, usize) {
    for frames in 1..=limit {
        app.update();
        let status = status(app, job);
        if status.state != "running" {
            return (status, frames);
        }
    }
    panic!("still running after {limit} frames: {:?}", status(app, job));
}

fn look_at(app: &mut App, params: Value) -> Value {
    let job = start(app, params);
    let (status, _) = run(app, &job, 20);
    assert_eq!(status.state, "succeeded", "{status:?}");
    status.result.unwrap()
}

pub(crate) fn pose(app: &App, camera: Entity) -> (DVec3, f64, f64) {
    let (position, rotation) = read_pose(app.world(), camera).unwrap();
    let (yaw, pitch) = yaw_pitch(rotation);
    (position, yaw, pitch)
}

#[test]
fn a_camera_with_leftover_velocity_stays_put_while_paused() {
    let mut app = app();
    let camera = editor_camera(&mut app);
    app.world_mut().resource_mut::<Time<Virtual>>().pause();
    *app.world_mut()
        .get_mut::<BigSpaceCameraController>(camera)
        .unwrap()
        .path_mut::<DVec3>("vel_translation")
        .unwrap() = DVec3::new(0.0, 0.0, -0.5);
    look_at(
        &mut app,
        json!({ "position": [5.0, 6.0, 7.0], "yaw": 0.0, "pitch": 0.0 }),
    );
    for _ in 0..30 {
        app.update();
    }
    let (position, _, _) = pose(&app, camera);
    assert!(
        (position - DVec3::new(5.0, 6.0, 7.0)).length() < 1e-4,
        "drifted to {position}"
    );
}

#[test]
fn the_default_guard_is_both() {
    let mut app = app_with_terrain();
    app.insert_resource(CameraGroundGuard::default());
    app.world_mut().resource_mut::<Terrain>().height = Some(5.0);
    let result = look_at(&mut app, json!({ "position": [0.0, 4.0, 0.0] }));
    assert_eq!(result["position"], json!([0.0, 7.0, 0.0]));
    assert_eq!(result["ground"]["mode"], "both");
    // A call can still turn it off.
    let result = look_at(
        &mut app,
        json!({ "position": [0.0, 4.0, 0.0], "ground": "none" }),
    );
    assert_eq!(result["position"], json!([0.0, 4.0, 0.0]));
    assert_eq!(result["ground"], Value::Null);
    assert_eq!(result["center_hit"], Value::Null);
}

#[test]
fn no_ground_is_not_waited_for() {
    let mut app = app_with_terrain();
    let job = start(&mut app, json!({ "position": [0.0, 3.0, 0.0] }));
    let (status, frames) = run(&mut app, &job, 20);
    assert_eq!(frames, 1);
    let result = status.result.unwrap();
    assert_eq!(result["ground"], Value::Null);
    assert!(
        result["notes"].to_string().contains("no ground"),
        "{result}"
    );
}

#[test]
fn ground_not_loaded_yet_is_waited_for() {
    let mut app = app_with_terrain();
    app.world_mut().resource_mut::<Terrain>().not_loaded = true;
    let job = start(&mut app, json!({ "position": [0.0, 1.0, 0.0] }));
    for _ in 0..5 {
        app.update();
    }
    assert_eq!(status(&app, &job).state, "running");
    let mut terrain = app.world_mut().resource_mut::<Terrain>();
    terrain.not_loaded = false;
    terrain.height = Some(3.0);
    let (status, _) = run(&mut app, &job, 5);
    assert_eq!(status.result.unwrap()["position"], json!([0.0, 5.0, 0.0]));
}

#[test]
fn the_newest_placement_wins() {
    let mut app = app_with_terrain();
    app.world_mut().resource_mut::<Terrain>().not_loaded = true;
    let first = start(&mut app, json!({ "position": [1.0, 1.0, 1.0] }));
    app.update();
    let second = start(&mut app, json!({ "position": [2.0, 1.0, 2.0] }));
    app.update();
    let mut terrain = app.world_mut().resource_mut::<Terrain>();
    terrain.not_loaded = false;
    terrain.height = Some(0.0);
    let (second_status, _) = run(&mut app, &second, 5);
    assert_eq!(second_status.state, "succeeded");
    let first_status = status(&app, &first);
    assert_eq!(first_status.error.unwrap().kind, "superseded");
    let camera = editor_camera(&mut app);
    let (position, _, _) = pose(&app, camera);
    assert!(
        (position - DVec3::new(2.0, 2.0, 2.0)).length() < 1e-4,
        "{position}"
    );
}

/// What closing a stage does to the editor camera: rescued off the grid, the grid and everything
/// under it (the orbit pivot included) despawned. Then the stage opens again.
fn reopen_stage(app: &mut App) {
    let root = app
        .world_mut()
        .query_filtered::<Entity, With<BigSpace>>()
        .single(app.world())
        .unwrap();
    app.world_mut().despawn(root);
    app.update();
    app.world_mut().spawn((
        BigSpace::default(),
        Grid::new(100.0, 0.0),
        Transform::default(),
        Name::new("stage"),
    ));
    app.update();
    app.update();
}

#[test]
fn placing_after_the_stage_is_reopened_moves_a_live_pivot() {
    let mut app = app();
    look_at(
        &mut app,
        json!({ "position": [10.0, 10.0, 10.0], "look_at": [10.0, 0.0, 0.0] }),
    );
    reopen_stage(&mut app);
    let camera = editor_camera(&mut app);
    let (focus, _) = orbit_focus(app.world(), camera).unwrap();
    assert!(
        app.world().get_entity(focus).is_ok(),
        "the pivot was replaced"
    );

    look_at(
        &mut app,
        json!({ "position": [20.0, 10.0, 10.0], "look_at": [20.0, 0.0, 0.0] }),
    );
    let (focus, distance) = orbit_focus(app.world(), camera).unwrap();
    let focus_position = grid::absolute_position(app.world(), focus)
        .unwrap()
        .position;
    assert!((focus_position - DVec3::new(20.0, 0.0, 0.0)).length() < 1e-3);
    assert!((distance - 200f32.sqrt()).abs() < 1e-3, "{distance}");
}

#[test]
fn a_non_finite_request_writes_nothing() {
    let mut app = app();
    let camera = editor_camera(&mut app);
    let before = pose(&app, camera);
    for request in [
        PoseRequest {
            position: Some(DVec3::new(f64::NAN, 1.0, 0.0)),
            aim: Aim::Keep,
            advance: 0.0,
        },
        PoseRequest {
            position: None,
            aim: Aim::Angles {
                yaw: Some(f64::INFINITY),
                pitch: None,
            },
            advance: 0.0,
        },
    ] {
        let options = PlaceOptions::new(app.world(), None, None, None).unwrap();
        let err = place_camera_pose(
            app.world_mut(),
            CameraChoice::EditorCamera,
            &request,
            options,
        )
        .err()
        .unwrap();
        assert_eq!(err.kind, "invalid_pose");
    }
    assert_eq!(pose(&app, camera), before);
}

#[test]
fn per_call_ground_and_clearance_override_the_game_guard() {
    let mut app = app_with_terrain();
    app.insert_resource(CameraGroundGuard {
        ground: GroundGuardMode::Meshes,
        min_clearance: 5.0,
    });
    app.world_mut().resource_mut::<Terrain>().height = Some(5.0);
    let result = look_at(&mut app, json!({ "position": [0.0, 4.0, 0.0] }));
    assert_eq!(result["position"], json!([0.0, 10.0, 0.0]), "{result}");
    assert_eq!(result["ground"]["mode"], "meshes");
    let result = look_at(
        &mut app,
        json!({ "position": [0.0, 4.0, 0.0], "ground": "both", "min_clearance": 1.0 }),
    );
    assert_eq!(result["position"], json!([0.0, 6.0, 0.0]), "{result}");
    assert_eq!(result["ground"]["mode"], "both");
}
