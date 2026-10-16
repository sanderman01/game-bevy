//! `screenshot.capture`: a screenshot as a job command.
//!
//! The job's work entity is the [`ScreenshotRequest`]. With `settle`, the entity first carries a
//! [`SettleGate`] and gets its request in the frame the gate opens. Cancelling despawns it, which
//! is best effort: a PNG already being encoded is still written after the job reports `cancelled`.

use std::path::PathBuf;

use bevy::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ScreenshotFinished, ScreenshotRequest, check_png_path};
use crate::{
    capture::{CaptureError, CaptureSources, restrict_source},
    command::{Call, CommandAppExt, CommandError, CommandSpec, JobHandle, Jobs, OnCancel},
    wait::{SettleGate, SettleParams, SettleTimedOut, Settled},
};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CaptureParams {
    /// Where to write the PNG. A relative path resolves against the caller's working directory.
    /// Missing directories are created and an existing file is replaced.
    path: PathBuf,
    /// What to capture. "window" is the whole game window, editor panels included.
    /// "game_camera" is what the stage's MainCamera rendered (in the editor, the Game View).
    /// Plugins add more, such as the editor's "editor_camera". A camera source needs its camera
    /// to be rendering.
    source: String,
    /// Wait for loading, pipelines and streaming to settle first, as `wait.settled` does; `{}`
    /// uses its defaults. The capture is taken in the frame it settles. Fails `not_settled` on
    /// timeout, without capturing.
    #[serde(default)]
    settle: Option<SettleParams>,
}

#[derive(Serialize, JsonSchema, Debug, PartialEq)]
struct Saved {
    /// Absolute.
    path: PathBuf,
    /// Physical pixels.
    width: u32,
    height: u32,
    source: String,
}

/// Registers `screenshot.capture`. Called from `ScreenshotPlugin::finish`, so every capture
/// source a plugin added in `build` is in the schema.
pub(super) fn register(app: &mut App) {
    let names = app
        .world()
        .get_resource::<CaptureSources>()
        .map(CaptureSources::names)
        .unwrap_or_default();
    app.register_job_command(
        CommandSpec::new(
            "screenshot.capture",
            "Capture the game window, the game camera or the editor camera to a PNG file. \
             Returns the absolute path written and its size in pixels.",
        )
        .adjust_params_schema(restrict_source(names)),
        capture,
    );
}

fn capture(
    In(call): In<Call<CaptureParams>>,
    world: &mut World,
) -> Result<JobHandle<Saved>, CommandError> {
    let path = call.resolve_path(&call.params.path)?;
    check_png_path(&path).map_err(|err| CommandError::invalid(err.kind(), err.to_string()))?;
    let name = call.params.source.clone();
    let source = CaptureSources::resolve(world, &name)?;
    let request = ScreenshotRequest { source, path };
    let gate = call
        .params
        .settle
        .as_ref()
        .map(|settle| SettleGate::new(settle, world))
        .transpose()?;
    let work = match gate {
        Some(gate) => world.spawn(gate).id(),
        None => world.spawn(request.clone()).id(),
    };
    let job = match call.start_job(world, work, OnCancel::Despawn) {
        Ok(job) => job,
        Err(err) => {
            world.despawn(work);
            return Err(err);
        }
    };
    let handle = job.clone();
    let timed_out = job.clone();
    world
        .entity_mut(work)
        .observe(move |settled: On<Settled>, mut commands: Commands| {
            commands.entity(settled.entity).insert(request.clone());
        })
        .observe(
            move |timeout: On<SettleTimedOut>, mut jobs: ResMut<Jobs>, mut commands: Commands| {
                jobs.finish(&timed_out, Err(timeout.error()));
                commands.entity(timeout.entity).despawn();
            },
        );
    world.entity_mut(work).observe(
        move |done: On<ScreenshotFinished>,
              mut jobs: ResMut<Jobs>,
              sources: Res<CaptureSources>| {
            let result = match &done.result {
                Ok(saved) => Ok(Saved {
                    path: saved.path.clone(),
                    width: saved.size.x,
                    height: saved.size.y,
                    source: name.clone(),
                }),
                Err(err) => Err(sources.error(&name, err)),
            };
            jobs.finish(&handle, result);
        },
    );
    Ok(job)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use bevy::camera::RenderTarget;
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        camera::MainCamera,
        capture::{CapturePlugin, CaptureSource},
        command::{
            Caller, CommandPlugin, CommandRegistry, CommandScope, CommandWorldExt, JobId, Reply,
        },
        screenshot::{SavedScreenshot, ScreenshotError},
    };

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((CommandPlugin, CapturePlugin));
        app.world_mut()
            .resource_mut::<CaptureSources>()
            .set_inactive_hint("game_camera", "show the Game View");
        register(&mut app);
        app
    }

    fn call(app: &mut App, params: Value) -> Result<Reply, CommandError> {
        app.world_mut().call_command(
            "screenshot.capture",
            params,
            Caller {
                scope: CommandScope::Development,
                cwd: Some("/project".into()),
            },
        )
    }

    fn started(app: &mut App, params: Value) -> (JobId, Entity) {
        let Reply::Job(job) = call(app, params).unwrap() else {
            panic!("not a job");
        };
        let request = app
            .world_mut()
            .query_filtered::<Entity, With<ScreenshotRequest>>()
            .single(app.world())
            .unwrap();
        (job, request)
    }

    fn status(app: &App, job: &JobId) -> crate::command::JobStatus {
        app.world()
            .resource::<Jobs>()
            .status(job, CommandScope::Development)
            .unwrap()
    }

    #[test]
    fn the_schema_lists_the_sources() {
        let app = app();
        let info = app
            .world()
            .resource::<CommandRegistry>()
            .get("screenshot.capture")
            .unwrap()
            .clone();
        assert_eq!(
            info.params["properties"]["source"]["enum"],
            json!(["game_camera", "window"])
        );
    }

    #[test]
    fn spawns_a_request_resolved_against_the_callers_directory() {
        let mut app = app();
        let (_, request) = started(
            &mut app,
            json!({ "path": "shots/a.png", "source": "window" }),
        );
        let request = app.world().get::<ScreenshotRequest>(request).unwrap();
        assert_eq!(request.source, CaptureSource::PrimaryWindow);
        assert_eq!(request.path, Path::new("/project/shots/a.png"));
    }

    #[test]
    fn refuses_bad_params_before_spawning() {
        let mut app = app();
        for (params, kind) in [
            (json!({ "path": "a.jpg", "source": "window" }), "not_png"),
            (
                json!({ "path": "a.png", "source": "moon" }),
                "unknown_source",
            ),
            (json!({ "path": "a.png" }), "invalid_params"),
            (
                json!({ "path": "a.png", "source": "game_camera" }),
                "no_main_camera",
            ),
        ] {
            let err = call(&mut app, params.clone()).unwrap_err();
            assert_eq!(err.kind, kind, "{params}");
        }
        let requests = app
            .world_mut()
            .query::<&ScreenshotRequest>()
            .iter(app.world())
            .count();
        assert_eq!(requests, 0);
    }

    #[test]
    fn the_finished_capture_finishes_the_job() {
        let mut app = app();
        let (job, request) = started(&mut app, json!({ "path": "/a.png", "source": "window" }));
        assert_eq!(status(&app, &job).state, "running");
        app.world_mut().trigger(ScreenshotFinished {
            entity: request,
            result: Ok(SavedScreenshot {
                path: "/a.png".into(),
                size: UVec2::new(4, 3),
            }),
        });
        let status = status(&app, &job);
        assert_eq!(status.state, "succeeded");
        assert_eq!(
            status.result,
            Some(json!({ "path": "/a.png", "width": 4, "height": 3, "source": "window" }))
        );
    }

    #[test]
    fn an_inactive_camera_fails_with_its_sources_hint() {
        let mut app = app();
        let camera = app
            .world_mut()
            .spawn((MainCamera, Camera::default(), RenderTarget::default()))
            .id();
        let (job, request) = started(
            &mut app,
            json!({ "path": "/a.png", "source": "game_camera" }),
        );
        app.world_mut().trigger(ScreenshotFinished {
            entity: request,
            result: Err(ScreenshotError::CameraInactive(camera)),
        });
        let error = status(&app, &job).error.unwrap();
        assert_eq!(error.kind, "camera_inactive");
        assert_eq!(error.hint.as_deref(), Some("show the Game View"));
    }

    #[test]
    fn cancelling_despawns_the_request() {
        let mut app = app();
        let (job, request) = started(&mut app, json!({ "path": "/a.png", "source": "window" }));
        let status = app
            .world_mut()
            .cancel_job(&job, CommandScope::Development)
            .unwrap();
        assert_eq!(status.state, "cancelled");
        assert!(app.world().get_entity(request).is_err());
    }

    mod settle {
        use bevy::window::PrimaryWindow;

        use super::*;
        use crate::{
            screenshot::{CaptureStage, ScreenshotPlugin},
            time::TimeControlPlugin,
            wait::{Activity, SettleAppExt, WaitPlugin},
        };

        #[derive(Resource, Default)]
        struct Busy(bool);

        fn busy_probe(busy: Res<Busy>) -> Activity {
            Activity::idle().pending("work", usize::from(busy.0))
        }

        fn app() -> App {
            let mut app = App::new();
            app.add_plugins((
                MinimalPlugins,
                TimeControlPlugin,
                CommandPlugin,
                CapturePlugin,
                WaitPlugin,
                ScreenshotPlugin,
            ))
            .insert_resource(Busy(true))
            .register_settle_probe("test", busy_probe);
            app.finish();
            app.cleanup();
            app.world_mut().spawn((Window::default(), PrimaryWindow));
            app
        }

        fn start(app: &mut App, settle: Value) -> JobId {
            let reply = call(
                app,
                json!({ "path": "/a.png", "source": "window", "settle": settle }),
            )
            .unwrap();
            let Reply::Job(job) = reply else {
                panic!("not a job");
            };
            job
        }

        fn requests(app: &mut App) -> Vec<(Entity, bool)> {
            app.world_mut()
                .query::<(Entity, &ScreenshotRequest, Option<&CaptureStage>)>()
                .iter(app.world())
                .map(|(entity, _, stage)| {
                    (entity, matches!(stage, Some(CaptureStage::Waiting { .. })))
                })
                .collect()
        }

        #[test]
        fn the_capture_is_requested_in_the_frame_it_settles() {
            let mut app = app();
            let job = start(&mut app, json!({ "stable_frames": 2, "probes": ["test"] }));
            for _ in 0..3 {
                app.update();
                assert!(requests(&mut app).is_empty(), "requested while busy");
            }
            let progress = status(&app, &job).progress.unwrap();
            assert_eq!(progress["phase"], "settling");
            assert_eq!(progress["busy"], json!({ "test": { "work": 1 } }));

            app.world_mut().resource_mut::<Busy>().0 = false;
            app.update();
            assert!(requests(&mut app).is_empty(), "one idle frame of two");
            app.update();
            // Settled and asked for its readback in the same `Last`.
            let requested = requests(&mut app);
            assert_eq!(requested.len(), 1);
            assert!(
                requested[0].1,
                "the readback was not requested in the settling frame"
            );
            assert_eq!(status(&app, &job).state, "running");
        }

        #[test]
        fn a_timeout_fails_and_captures_nothing() {
            let mut app = app();
            let job = start(&mut app, json!({ "timeout_seconds": 0.01 }));
            app.update();
            std::thread::sleep(std::time::Duration::from_millis(20));
            app.update();
            app.update();
            let error = status(&app, &job).error.unwrap();
            assert_eq!(error.kind, "not_settled");
            assert!(error.message.contains("test (work 1)"), "{}", error.message);
            assert!(requests(&mut app).is_empty());
        }

        #[test]
        fn cancelling_while_settling_ends_the_job() {
            let mut app = app();
            let job = start(&mut app, json!({}));
            app.update();
            let status = app
                .world_mut()
                .cancel_job(&job, CommandScope::Development)
                .unwrap();
            assert_eq!(status.state, "cancelled");
            app.world_mut().resource_mut::<Busy>().0 = false;
            for _ in 0..10 {
                app.update();
            }
            assert!(requests(&mut app).is_empty());
        }
    }
}
