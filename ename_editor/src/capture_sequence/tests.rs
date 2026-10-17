use std::path::{Path, PathBuf};

use bevy::math::DVec3;
use ename_engine::{
    camera::viewpoint::{self, Viewpoint},
    command::{CommandScope, CommandWorldExt, JobId, JobStatus, Reply},
    time::TimeControlPlugin,
    video::VideoEnd,
    wait::{Activity, SettleAppExt},
};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

use super::*;
use crate::{
    camera_commands::tests::{base_app, call, editor_camera, finish, pose},
    viewpoints::ViewpointsPlugin,
};

#[derive(Resource, Default)]
struct Busy(bool);

fn busy_probe(busy: Res<Busy>) -> Activity {
    Activity::idle().pending("work", usize::from(busy.0))
}

fn app(dir: &TempDir) -> App {
    let mut app = base_app();
    app.add_plugins((
        TimeControlPlugin,
        ViewpointsPlugin {
            file: Some(dir.path().join("viewpoints.ron")),
        },
        CaptureSequencePlugin,
    ))
    .init_resource::<Busy>()
    .register_settle_probe("test", busy_probe);
    finish(app)
}

fn save(dir: &TempDir, name: &str, position: [f64; 3], yaw: f64) {
    viewpoint::save(
        &dir.path().join("viewpoints.ron"),
        Viewpoint {
            name: name.to_owned(),
            note: String::new(),
            position,
            yaw,
            pitch: 0.0,
            facets: Default::default(),
        },
        false,
    )
    .unwrap();
}

fn start(app: &mut App, name: &str, params: Value) -> JobId {
    match call(app, name, params).unwrap() {
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

fn requests(app: &mut App) -> Vec<(Entity, PathBuf)> {
    app.world_mut()
        .query::<(Entity, &ScreenshotRequest)>()
        .iter(app.world())
        .map(|(entity, request)| (entity, request.path.clone()))
        .collect()
}

/// What the screenshot plugin does: report and despawn each request. Records where the camera
/// was when each was asked for.
fn deliver(app: &mut App, size: UVec2, seen: &mut Vec<(PathBuf, DVec3)>) {
    for (entity, path) in requests(app) {
        let camera = editor_camera(app);
        seen.push((path.clone(), pose(app, camera).0));
        app.world_mut().trigger(ScreenshotFinished {
            entity,
            result: Ok(SavedScreenshot { path, size }),
        });
        app.world_mut().despawn(entity);
    }
}

/// Runs the job to its end, delivering captures at `size`.
fn drive(app: &mut App, job: &JobId, size: UVec2) -> (JobStatus, Vec<(PathBuf, DVec3)>) {
    let mut seen = Vec::new();
    for _ in 0..200 {
        app.update();
        deliver(app, size, &mut seen);
        let status = status(app, job);
        if status.state != "running" {
            return (status, seen);
        }
    }
    panic!("still running: {:?}", status(app, job));
}

fn manifest(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn two_viewpoints() -> (TempDir, App) {
    let dir = tempdir().unwrap();
    save(&dir, "start", [10.0, 5.0, 0.0], 0.0);
    save(&dir, "far shore", [50.0, 8.0, 20.0], 90.0);
    let app = app(&dir);
    (dir, app)
}

#[test]
fn captures_each_viewpoint_and_puts_everything_back() {
    let (dir, mut app) = two_viewpoints();
    let camera = editor_camera(&mut app);
    let (before, _, _) = pose(&app, camera);
    let out = dir.path().join("shots");
    let job = start(
        &mut app,
        "capture.viewpoints",
        json!({ "viewpoints": ["far shore", "start"], "dir": out, "source": "editor_camera" }),
    );
    assert!(app.world().resource::<Time<Virtual>>().is_paused());
    assert!(app.world().contains_resource::<OverlaysHidden>());
    let (status, seen) = drive(&mut app, &job, UVec2::new(8, 6));
    assert_eq!(status.state, "succeeded", "{status:?}");
    let result = status.result.unwrap();
    assert_eq!(
        result["shots"],
        json!([{ "viewpoint": "far shore", "file": "far_shore.png" },
               { "viewpoint": "start", "file": "start.png" }])
    );
    assert_eq!(
        (result["width"].clone(), result["height"].clone()),
        (json!(8), json!(6))
    );
    assert_eq!(result["size_changed"], false);
    // Each capture was asked for with its own pose in place.
    assert_eq!(seen.len(), 2);
    assert!(
        (seen[0].1 - DVec3::new(50.0, 8.0, 20.0)).length() < 1e-3,
        "{seen:?}"
    );
    assert!(
        (seen[1].1 - DVec3::new(10.0, 5.0, 0.0)).length() < 1e-3,
        "{seen:?}"
    );

    let manifest = manifest(&out.join("manifest.json"));
    assert_eq!(manifest["source"], "editor_camera");
    assert_eq!(manifest["frames"][0]["file"], "far_shore.png");
    assert_eq!(manifest["frames"][0]["viewpoint"], "far shore");
    assert_eq!(manifest["frames"][1]["position"], json!([10.0, 5.0, 0.0]));
    assert_eq!(manifest["frames"][1]["width"], 8);

    let (after, _, _) = pose(&app, camera);
    assert!((after - before).length() < 1e-3, "{after} vs {before}");
    assert!(!app.world().resource::<Time<Virtual>>().is_paused());
    assert!(!app.world().contains_resource::<OverlaysHidden>());
    assert!(app.world().resource::<RunningSequence>().0.is_none());
}

#[test]
fn refuses_bad_calls_before_anything_moves() {
    let (dir, mut app) = two_viewpoints();
    let out = dir.path().join("shots");
    for (params, kind) in [
        (
            json!({ "viewpoints": ["start", "nope"] }),
            "unknown_viewpoint",
        ),
        (
            json!({ "viewpoints": ["start", "start"] }),
            "duplicate_viewpoint",
        ),
        (json!({ "viewpoints": [] }), "invalid_params"),
    ] {
        let mut params = params;
        params["dir"] = json!(out);
        params["source"] = json!("editor_camera");
        let err = call(&mut app, "capture.viewpoints", params).unwrap_err();
        assert_eq!(err.kind, kind);
    }
    let err = call(
        &mut app,
        "capture.viewpoints",
        json!({ "viewpoints": ["start"], "dir": out, "source": "window" }),
    )
    .unwrap_err();
    assert_eq!(err.kind, "invalid_params");
    assert!(!app.world().resource::<Time<Virtual>>().is_paused());
    assert!(!out.exists());
}

#[test]
fn a_used_directory_needs_replace_which_deletes_only_listed_files() {
    let (dir, mut app) = two_viewpoints();
    let out = dir.path().join("shots");
    let job = start(
        &mut app,
        "capture.viewpoints",
        json!({ "viewpoints": ["start"], "dir": out, "source": "editor_camera" }),
    );
    std::fs::create_dir_all(&out).unwrap();
    drive(&mut app, &job, UVec2::new(4, 4));
    std::fs::write(out.join("start.png"), b"old").unwrap();
    std::fs::write(out.join("notes.txt"), b"mine").unwrap();
    let again = json!({ "viewpoints": ["far shore"], "dir": out, "source": "editor_camera" });
    let err = call(&mut app, "capture.viewpoints", again.clone()).unwrap_err();
    assert_eq!(err.kind, "dir_not_empty");

    let mut replace = again;
    replace["replace"] = json!(true);
    let job = start(&mut app, "capture.viewpoints", replace);
    assert!(
        !out.join("start.png").exists(),
        "listed in the old manifest"
    );
    assert!(out.join("notes.txt").exists(), "not listed");
    let (status, _) = drive(&mut app, &job, UVec2::new(4, 4));
    assert_eq!(status.state, "succeeded");
    let manifest = manifest(&out.join("manifest.json"));
    assert_eq!(manifest["frames"].as_array().unwrap().len(), 1);
}

#[test]
fn one_sequence_at_a_time_and_views_wait_for_it() {
    let (dir, mut app) = two_viewpoints();
    let job = start(
        &mut app,
        "capture.viewpoints",
        json!({ "viewpoints": ["start"], "dir": dir.path().join("a"), "source": "editor_camera" }),
    );
    let err = call(
        &mut app,
        "capture.viewpoints",
        json!({ "viewpoints": ["start"], "dir": dir.path().join("b"), "source": "editor_camera" }),
    )
    .unwrap_err();
    assert_eq!(err.kind, "busy");
    assert!(err.message.contains(job.as_str()), "{err}");
    let err = call(&mut app, "viewpoint.goto", json!({ "name": "start" })).unwrap_err();
    assert_eq!(err.kind, "busy");
    drive(&mut app, &job, UVec2::new(4, 4));
    assert!(call(&mut app, "viewpoint.goto", json!({ "name": "start" })).is_ok());
}

#[test]
fn a_step_that_does_not_settle_is_skipped_and_listed() {
    let (dir, mut app) = two_viewpoints();
    let out = dir.path().join("shots");
    app.world_mut().resource_mut::<Busy>().0 = true;
    let job = start(
        &mut app,
        "capture.viewpoints",
        json!({ "viewpoints": ["start", "far shore"], "dir": out, "source": "editor_camera",
                "settle": { "probes": ["test"], "timeout_seconds": 0.05 } }),
    );
    let mut seen = Vec::new();
    for _ in 0..200 {
        app.update();
        deliver(&mut app, UVec2::new(4, 4), &mut seen);
        // Busy past the first step's timeout, then idle.
        if !app
            .world()
            .resource::<Jobs>()
            .status(&job, CommandScope::Development)
            .unwrap()
            .progress
            .is_some_and(|progress| progress["done"] == 0)
        {
            app.world_mut().resource_mut::<Busy>().0 = false;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        if status(&app, &job).state != "running" {
            break;
        }
    }
    let status = status(&app, &job);
    assert_eq!(status.state, "succeeded", "{status:?}");
    let result = status.result.unwrap();
    assert_eq!(result["failed"][0]["viewpoint"], "start");
    assert_eq!(result["failed"][0]["error"], "not_settled");
    assert_eq!(
        result["failed"][0]["busy"],
        json!({ "test": { "work": 1 } })
    );
    assert_eq!(
        result["shots"],
        json!([{ "viewpoint": "far shore", "file": "far_shore.png" }])
    );
    assert_eq!(seen.len(), 1);
    // The manifest lists the skipped step too, with why.
    let manifest = manifest(&out.join("manifest.json"));
    assert_eq!(manifest["frames"][0]["viewpoint"], "start");
    assert_eq!(manifest["frames"][0]["error"], "not_settled");
    assert!(manifest["frames"][0].get("file").is_none());
    assert_eq!(manifest["frames"][1]["file"], "far_shore.png");
}

#[test]
fn cancel_waits_for_the_capture_in_flight_then_restores() {
    let (dir, mut app) = two_viewpoints();
    let out = dir.path().join("shots");
    let camera = editor_camera(&mut app);
    let (before, _, _) = pose(&app, camera);
    let job = start(
        &mut app,
        "capture.viewpoints",
        json!({ "viewpoints": ["start", "far shore"], "dir": out, "source": "editor_camera" }),
    );
    while requests(&mut app).is_empty() {
        app.update();
    }
    let cancelled = app
        .world_mut()
        .cancel_job(&job, CommandScope::Development)
        .unwrap();
    assert_eq!(cancelled.state, "running", "waits for the capture");
    app.update();
    assert_eq!(status(&app, &job).state, "running");
    let mut seen = Vec::new();
    deliver(&mut app, UVec2::new(4, 4), &mut seen);
    app.update();
    assert_eq!(status(&app, &job).state, "cancelled");
    let manifest = manifest(&out.join("manifest.json"));
    assert_eq!(manifest["frames"].as_array().unwrap().len(), 1);
    let (after, _, _) = pose(&app, camera);
    assert!((after - before).length() < 1e-3);
    assert!(!app.world().resource::<Time<Virtual>>().is_paused());
    assert!(requests(&mut app).is_empty());
}

#[test]
fn a_resize_between_frames_is_reported() {
    let (dir, mut app) = two_viewpoints();
    let out = dir.path().join("shots");
    let job = start(
        &mut app,
        "capture.viewpoints",
        json!({ "viewpoints": ["start", "far shore"], "dir": out, "source": "editor_camera" }),
    );
    let mut seen = Vec::new();
    let mut sizes = [UVec2::new(8, 6), UVec2::new(6, 6)].into_iter();
    let mut size = sizes.next().unwrap();
    for _ in 0..200 {
        app.update();
        if !requests(&mut app).is_empty() {
            deliver(&mut app, size, &mut seen);
            size = sizes.next().unwrap_or(size);
        }
        if status(&app, &job).state != "running" {
            break;
        }
    }
    let result = status(&app, &job).result.unwrap();
    assert_eq!(result["size_changed"], true);
    assert_eq!(result["width"], 8);
    let manifest = manifest(&out.join("manifest.json"));
    assert_eq!(manifest["frames"][1]["width"], 6);
}

#[test]
fn another_placement_supersedes_the_sequence_and_keeps_its_pose() {
    let (dir, mut app) = two_viewpoints();
    let out = dir.path().join("shots");
    let job = start(
        &mut app,
        "capture.viewpoints",
        json!({ "viewpoints": ["start", "far shore"], "dir": out, "source": "editor_camera" }),
    );
    let mut seen = Vec::new();
    while seen.is_empty() {
        app.update();
        deliver(&mut app, UVec2::new(4, 4), &mut seen);
    }
    let look = start(
        &mut app,
        "camera.look_at",
        json!({ "position": [1.0, 2.0, 3.0], "ground": "none" }),
    );
    let (status, _) = drive(&mut app, &job, UVec2::new(4, 4));
    assert_eq!(status.error.unwrap().kind, "superseded");
    for _ in 0..5 {
        app.update();
    }
    assert_eq!(self::status(&app, &look).state, "succeeded");
    let camera = editor_camera(&mut app);
    let (after, _, _) = pose(&app, camera);
    assert!(
        (after - DVec3::new(1.0, 2.0, 3.0)).length() < 1e-3,
        "{after}"
    );
    assert!(!app.world().resource::<Time<Virtual>>().is_paused());
}

#[test]
fn a_fly_through_writes_padded_stills_along_the_path() {
    let (dir, mut app) = two_viewpoints();
    let out = dir.path().join("fly");
    let job = start(
        &mut app,
        "capture.fly_through",
        json!({ "source": "editor_camera", "steps": 11,
                "path": { "from": "start", "along": [1.0, 0.0, 0.0], "distance": 100.0 },
                "record": { "stills": { "dir": out } } }),
    );
    let (status, seen) = drive(&mut app, &job, UVec2::new(4, 4));
    assert_eq!(status.state, "succeeded", "{status:?}");
    assert_eq!(status.result.unwrap()["frames"], 11);
    assert!(
        seen[0].0.ends_with("00.png") && seen[10].0.ends_with("10.png"),
        "{seen:?}"
    );
    assert!(
        (seen[5].1 - DVec3::new(60.0, 5.0, 0.0)).length() < 1e-3,
        "{seen:?}"
    );
}

#[test]
fn a_video_fly_through_follows_the_recorder() {
    let (dir, mut app) = two_viewpoints();
    let path = dir.path().join("fly.mp4");
    let camera = editor_camera(&mut app);
    let (before, _, _) = pose(&app, camera);
    let job = start(
        &mut app,
        "capture.fly_through",
        json!({ "source": "editor_camera", "steps": 3,
                "path": { "from": "start", "to": { "position": [30.0, 5.0, 0.0], "yaw": 0.0, "pitch": 0.0 } },
                "record": { "video": { "path": path } } }),
    );
    let request = loop {
        app.update();
        let found = app
            .world_mut()
            .query_filtered::<Entity, With<VideoRequest>>()
            .iter(app.world())
            .next();
        if let Some(request) = found {
            break request;
        }
    };
    let x = |app: &App| pose(app, camera).0.x;
    // Starting: nothing requested, so pose 0 holds.
    app.update();
    assert!((x(&app) - 10.0).abs() < 1e-3);
    for (requested, expected) in [(1, 20.0), (2, 30.0), (3, 30.0)] {
        app.world_mut()
            .get_mut::<VideoProgress>(request)
            .unwrap()
            .requested = requested;
        app.update();
        assert!(
            (x(&app) - expected).abs() < 1e-3,
            "{requested}: {}",
            x(&app)
        );
    }
    app.world_mut().trigger(VideoFinished {
        entity: request,
        result: Ok(SavedVideo {
            path: path.clone(),
            size: UVec2::new(8, 6),
            frames: 3,
            frame_rate: 30,
            duration: std::time::Duration::from_millis(100),
            dropped_frames: 0,
            encoder: "test".into(),
            ended: VideoEnd::Completed,
        }),
    });
    app.update();
    let status = status(&app, &job);
    assert_eq!(status.state, "succeeded", "{status:?}");
    let result = status.result.unwrap();
    assert_eq!(
        (result["frames"].clone(), result["ended"].clone()),
        (json!(3), json!("completed"))
    );
    let manifest = manifest(&dir.path().join("fly.mp4.manifest.json"));
    assert_eq!(manifest["frames"].as_array().unwrap().len(), 3);
    assert_eq!(manifest["width"], 8);
    assert!((pose(&app, camera).0 - before).length() < 1e-3, "restored");
}

#[test]
fn file_names_are_portable_and_unique() {
    let mut taken = Vec::new();
    let names: Vec<String> = ["far shore", "Far_Shore", "con", "lpt1.x", "dots..", "a/b"]
        .iter()
        .map(|name| file_name_for(name, &mut taken))
        .collect();
    assert_eq!(
        names,
        [
            "far_shore.png",
            "Far_Shore-2.png",
            "_con.png",
            "_lpt1.x.png",
            "dots.png",
            "a_b.png"
        ]
    );
}

#[test]
fn replace_never_touches_what_the_old_manifest_does_not_own() {
    let (dir, mut app) = two_viewpoints();
    let out = dir.path().join("shots");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("keep.png"), b"mine").unwrap();
    std::fs::write(dir.path().join("outside.png"), b"mine").unwrap();
    std::fs::write(
        out.join("manifest.json"),
        json!({ "run": "x", "source": "editor_camera", "width": 1, "height": 1,
                "virtual_time": 0.0, "frames": [
                    { "file": "../outside.png", "position": [0.0, 0.0, 0.0], "yaw": 0.0, "pitch": 0.0, "frame": 0 },
                ] })
        .to_string(),
    )
    .unwrap();
    // A file this run would write that the manifest does not list is refused, before deleting.
    std::fs::write(out.join("Start.png"), b"mine").unwrap();
    let params = json!({ "viewpoints": ["start"], "dir": out, "source": "editor_camera",
                         "replace": true });
    let err = call(&mut app, "capture.viewpoints", params.clone()).unwrap_err();
    assert_eq!(err.kind, "file_exists");
    assert!(out.join("manifest.json").exists());
    assert!(!app.world().resource::<Time<Virtual>>().is_paused());

    std::fs::remove_file(out.join("Start.png")).unwrap();
    let job = start(&mut app, "capture.viewpoints", params);
    let (status, _) = drive(&mut app, &job, UVec2::new(4, 4));
    assert_eq!(status.state, "succeeded");
    assert!(dir.path().join("outside.png").exists(), "not a plain name");
    assert!(out.join("keep.png").exists(), "not listed");
}

#[test]
fn cancel_while_settling_ends_at_once_and_restores() {
    let (dir, mut app) = two_viewpoints();
    app.world_mut().resource_mut::<Busy>().0 = true;
    let job = start(
        &mut app,
        "capture.viewpoints",
        json!({ "viewpoints": ["start"], "dir": dir.path().join("s"), "source": "editor_camera",
                "settle": { "probes": ["test"] } }),
    );
    for _ in 0..5 {
        app.update();
    }
    app.world_mut()
        .cancel_job(&job, CommandScope::Development)
        .unwrap();
    app.update();
    assert_eq!(status(&app, &job).state, "cancelled");
    assert!(
        app.world_mut()
            .query::<&SettleGate>()
            .iter(app.world())
            .next()
            .is_none()
    );
    assert!(!app.world().resource::<Time<Virtual>>().is_paused());
    assert!(call(&mut app, "viewpoint.goto", json!({ "name": "start" })).is_ok());
}

#[test]
fn a_placement_during_a_cancel_keeps_its_pose() {
    let (dir, mut app) = two_viewpoints();
    let job = start(
        &mut app,
        "capture.viewpoints",
        json!({ "viewpoints": ["start", "far shore"], "dir": dir.path().join("c"),
                "source": "editor_camera" }),
    );
    while requests(&mut app).is_empty() {
        app.update();
    }
    app.world_mut()
        .cancel_job(&job, CommandScope::Development)
        .unwrap();
    app.update();
    // Placed while the cancel waits for the capture in flight.
    start(
        &mut app,
        "camera.look_at",
        json!({ "position": [1.0, 2.0, 3.0], "ground": "none" }),
    );
    let mut seen = Vec::new();
    deliver(&mut app, UVec2::new(4, 4), &mut seen);
    for _ in 0..3 {
        app.update();
    }
    assert_eq!(status(&app, &job).state, "cancelled");
    let camera = editor_camera(&mut app);
    assert!((pose(&app, camera).0 - DVec3::new(1.0, 2.0, 3.0)).length() < 1e-3);
}

/// Starts a 100-step video and returns the job and the recording's request.
fn start_video(app: &mut App, dir: &TempDir) -> (JobId, Entity) {
    let job = start(
        app,
        "capture.fly_through",
        json!({ "source": "editor_camera", "steps": 100,
                "path": { "from": "start", "along": "up", "distance": 10.0 },
                "record": { "video": { "path": dir.path().join("v.mp4") } } }),
    );
    for _ in 0..20 {
        app.update();
        let found = app
            .world_mut()
            .query_filtered::<Entity, With<VideoRequest>>()
            .iter(app.world())
            .next();
        if let Some(request) = found {
            return (job, request);
        }
    }
    panic!("no recording started");
}

#[derive(Resource, Default)]
struct StopsSeen(u32);

#[test]
fn cancelling_a_video_stops_it_and_keeps_only_recorded_frames() {
    for frames in [0, 7] {
        let (dir, mut app) = two_viewpoints();
        app.init_resource::<StopsSeen>().add_observer(
            |_: On<VideoStopRequested>, mut stops: ResMut<StopsSeen>| {
                stops.0 += 1;
            },
        );
        let (job, request) = start_video(&mut app, &dir);
        if frames > 0 {
            app.world_mut()
                .get_mut::<VideoProgress>(request)
                .unwrap()
                .requested = frames;
            app.update();
        }
        app.world_mut()
            .cancel_job(&job, CommandScope::Development)
            .unwrap();
        app.update();
        app.update();
        assert_eq!(
            app.world().resource::<StopsSeen>().0,
            1,
            "{frames}: one stop"
        );
        assert_eq!(status(&app, &job).state, "running", "waits for the file");
        let result = if frames == 0 {
            Err(ename_engine::video::VideoError::NoFrames(VideoEnd::Stopped))
        } else {
            Ok(SavedVideo {
                path: dir.path().join("v.mp4"),
                size: UVec2::new(8, 6),
                frames,
                frame_rate: 30,
                duration: std::time::Duration::from_millis(200),
                dropped_frames: 0,
                encoder: "test".into(),
                ended: VideoEnd::Stopped,
            })
        };
        app.world_mut().trigger(VideoFinished {
            entity: request,
            result,
        });
        app.update();
        assert_eq!(status(&app, &job).state, "cancelled", "{frames}");
        let manifest = manifest(&dir.path().join("v.mp4.manifest.json"));
        assert_eq!(
            manifest["frames"].as_array().unwrap().len(),
            frames as usize,
            "{frames}"
        );
    }
}
