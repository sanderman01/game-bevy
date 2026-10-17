//! The editor's commands: `editor.get_view` and `editor.set_view`, which of Scene View and Game
//! View is shown. Also the `editor_camera` capture source, and the hints that tell a caller which
//! view makes a camera render.
//!
//! # Timing
//!
//! Commands run in `RemoteLast` when called over BRP, after `Last`. `set_view` applies the dock's
//! selection at once, so `get_view` straight after reports it. The cameras follow on the next
//! frame's egui pass, so `set_view` is a job: [`settle_views`] finishes it once the target view's
//! camera is active and one more frame has run, which lets the render world, a frame behind under
//! pipelined rendering, draw with it. A Game View with no game camera never becomes active, so
//! the job also finishes after [`SETTLE_CAP`] frames with whatever is shown.
//!
//! Cancelling `set_view` stops only the wait. The view has already changed.

use bevy::{diagnostic::FrameCount, prelude::*};
use ename_engine::{
    camera::single_main_camera,
    capture::{CaptureSource, CaptureSources},
    command::{
        Call, CommandAppExt, CommandError, CommandSpec, JobHandle, Jobs, NoParams, OnCancel,
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    camera::EditorCamera,
    panels::{ActiveViewport, UiState},
};

/// Frames `set_view` waits at most for the target camera to become active.
const SETTLE_CAP: u32 = 10;

/// Registers the view commands, the `editor_camera` source and the inactive-camera hints.
pub(crate) struct ViewCommandsPlugin;

impl Plugin for ViewCommandsPlugin {
    fn build(&self, app: &mut App) {
        {
            let mut sources = app.world_mut().get_resource_or_init::<CaptureSources>();
            sources.insert("editor_camera", editor_camera_source);
            sources.set_inactive_hint(
                "editor_camera",
                "The Scene View's camera only renders while the Scene View is shown: call \
                 `editor.set_view` with view=scene first.",
            );
            sources.set_inactive_hint(
                "game_camera",
                "The game camera only renders while the Game View is shown: call \
                 `editor.set_view` with view=game first.",
            );
        }
        app.register_command(
            CommandSpec::new(
                "editor.get_view",
                "Which editor view is shown, Scene View (the editor's free camera) or Game View \
                 (the stage's MainCamera), and whether the Game View has a camera to show.",
            )
            .read_only(),
            get_view,
        )
        .register_job_command(
            CommandSpec::new(
                "editor.set_view",
                "Show the editor's Scene View or Game View, as clicking its tab would. Finishes \
                 once the game has drawn a frame with it, so a screenshot taken next shows it.",
            ),
            set_view,
        )
        .add_systems(Last, settle_views);
    }
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum View {
    Scene,
    Game,
}

impl From<ActiveViewport> for View {
    fn from(viewport: ActiveViewport) -> Self {
        match viewport {
            ActiveViewport::Scene => Self::Scene,
            ActiveViewport::Game => Self::Game,
        }
    }
}

impl From<View> for ActiveViewport {
    fn from(view: View) -> Self {
        match view {
            View::Scene => Self::Scene,
            View::Game => Self::Game,
        }
    }
}

#[derive(Serialize, JsonSchema, Debug, PartialEq)]
struct ViewState {
    view: View,
    /// Whether the Game View has a camera to show. False makes it show a placeholder instead.
    game_camera: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetViewParams {
    /// "scene" is the editor's own free camera, "game" is the loaded stage's MainCamera.
    view: View,
}

/// A `set_view` job waiting for its view's camera.
#[derive(Component)]
struct ViewSettle {
    job: JobHandle<ViewState>,
    target: ActiveViewport,
    /// Frame the job started in.
    started: u32,
    /// First frame the target camera was seen active.
    active_since: Option<u32>,
}

fn get_view(In(_): In<Call<NoParams>>, world: &mut World) -> Result<ViewState, CommandError> {
    Ok(read_view(world))
}

fn set_view(
    In(call): In<Call<SetViewParams>>,
    world: &mut World,
) -> Result<JobHandle<ViewState>, CommandError> {
    crate::capture_sequence::check_not_busy(world)?;
    let target = ActiveViewport::from(call.params.view);
    world
        .resource_mut::<UiState>()
        .show_viewport(target)
        .map_err(|message| CommandError::failed("view_unavailable", message))?;
    let started = frame(world);
    let work = world.spawn_empty().id();
    let job = match call.start_job(world, work, OnCancel::Despawn) {
        Ok(job) => job,
        Err(err) => {
            world.despawn(work);
            return Err(err);
        }
    };
    world.entity_mut(work).insert(ViewSettle {
        job: job.clone(),
        target,
        started,
        active_since: None,
    });
    Ok(job)
}

/// Finishes `set_view` jobs whose camera has rendered, or that waited [`SETTLE_CAP`] frames.
fn settle_views(world: &mut World) {
    let now = frame(world);
    let waiting: Vec<(Entity, ActiveViewport)> = world
        .query::<(Entity, &ViewSettle)>()
        .iter(world)
        .map(|(entity, settle)| (entity, settle.target))
        .collect();
    for (entity, target) in waiting {
        let active = camera_active(world, target);
        let mut settle = world.get_mut::<ViewSettle>(entity).unwrap();
        if active && settle.active_since.is_none() {
            settle.active_since = Some(now);
        }
        let rendered = settle.active_since.is_some_and(|since| now > since);
        let capped = now.wrapping_sub(settle.started) >= SETTLE_CAP;
        if !(rendered || capped) {
            continue;
        }
        let job = settle.job.clone();
        let shown = read_view(world);
        world.resource_mut::<Jobs>().finish(&job, Ok(shown));
        world.despawn(entity);
    }
}

fn camera_active(world: &mut World, viewport: ActiveViewport) -> bool {
    let camera = match viewport {
        ActiveViewport::Scene => world
            .query_filtered::<Entity, With<EditorCamera>>()
            .single(world)
            .ok(),
        ActiveViewport::Game => single_main_camera(world).ok(),
    };
    camera
        .and_then(|camera| world.get::<Camera>(camera))
        .is_some_and(|camera| camera.is_active)
}

fn frame(world: &World) -> u32 {
    world
        .get_resource::<FrameCount>()
        .map_or(0, |count| count.0)
}

fn read_view(world: &mut World) -> ViewState {
    ViewState {
        view: world.resource::<UiState>().active_viewport.into(),
        game_camera: single_main_camera(world).is_ok(),
    }
}

fn editor_camera_source(world: &mut World) -> Result<CaptureSource, CommandError> {
    world
        .query_filtered::<Entity, With<EditorCamera>>()
        .single(world)
        .map(CaptureSource::Camera)
        .map_err(|err| {
            CommandError::failed(
                "no_editor_camera",
                format!("the editor camera is missing: {err}"),
            )
        })
}

#[cfg(test)]
mod tests {
    use bevy::camera::RenderTarget;
    use ename_engine::{
        camera::MainCamera,
        capture::{CapturePlugin, CaptureSource},
        command::{Caller, CommandPlugin, CommandScope, CommandWorldExt, JobId, JobStatus, Reply},
    };
    use serde_json::{Value, json};

    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((CommandPlugin, CapturePlugin, ViewCommandsPlugin))
            .insert_resource(UiState::new())
            .init_resource::<FrameCount>();
        app
    }

    fn call(app: &mut App, name: &str, params: Value) -> Reply {
        app.world_mut()
            .call_command(
                name,
                params,
                Caller {
                    scope: CommandScope::Development,
                    cwd: None,
                },
            )
            .unwrap()
    }

    /// One frame: the counter moves on, then `Last` runs.
    fn frame(app: &mut App) {
        app.world_mut().resource_mut::<FrameCount>().0 += 1;
        app.update();
    }

    fn status(app: &App, job: &JobId) -> JobStatus {
        app.world()
            .resource::<Jobs>()
            .status(job, CommandScope::Development)
            .unwrap()
    }

    fn set(app: &mut App, view: &str) -> JobId {
        match call(app, "editor.set_view", json!({ "view": view })) {
            Reply::Job(job) => job,
            Reply::Done(value) => panic!("not a job: {value}"),
        }
    }

    #[test]
    fn get_reports_the_view_and_whether_there_is_a_game_camera() {
        let mut app = app();
        assert_eq!(
            call(&mut app, "editor.get_view", json!({})),
            Reply::Done(json!({ "view": "scene", "game_camera": false }))
        );
        app.world_mut()
            .spawn((MainCamera, Camera::default(), RenderTarget::default()));
        assert_eq!(
            call(&mut app, "editor.get_view", Value::Null),
            Reply::Done(json!({ "view": "scene", "game_camera": true }))
        );
    }

    #[test]
    fn set_finishes_a_frame_after_the_camera_is_active() {
        let mut app = app();
        let camera = app
            .world_mut()
            .spawn((
                MainCamera,
                Camera {
                    is_active: false,
                    ..default()
                },
                RenderTarget::default(),
            ))
            .id();
        let job = set(&mut app, "game");
        // Applied at once.
        assert_eq!(
            app.world().resource::<UiState>().active_viewport,
            ActiveViewport::Game
        );
        frame(&mut app);
        assert_eq!(status(&app, &job).state, "running");
        // What the Game View does on the next egui pass.
        app.world_mut().get_mut::<Camera>(camera).unwrap().is_active = true;
        frame(&mut app);
        assert_eq!(status(&app, &job).state, "running");
        frame(&mut app);
        let status = status(&app, &job);
        assert_eq!(status.state, "succeeded");
        assert_eq!(
            status.result,
            Some(json!({ "view": "game", "game_camera": true }))
        );
    }

    #[test]
    fn set_finishes_at_the_cap_when_the_camera_never_renders() {
        let mut app = app();
        let job = set(&mut app, "game");
        for _ in 0..SETTLE_CAP - 1 {
            frame(&mut app);
        }
        assert_eq!(status(&app, &job).state, "running");
        frame(&mut app);
        assert_eq!(
            status(&app, &job).result,
            Some(json!({ "view": "game", "game_camera": false }))
        );
    }

    #[test]
    fn an_unknown_view_is_invalid_params() {
        let mut app = app();
        let err = app
            .world_mut()
            .call_command(
                "editor.set_view",
                json!({ "view": "top" }),
                Caller {
                    scope: CommandScope::Development,
                    cwd: None,
                },
            )
            .unwrap_err();
        assert_eq!(err.kind, "invalid_params");
    }

    #[test]
    fn the_editor_camera_is_a_source_and_hints_survive_plugin_order() {
        for editor_first in [true, false] {
            let mut app = App::new();
            if editor_first {
                app.add_plugins((ViewCommandsPlugin, CapturePlugin, CommandPlugin));
            } else {
                app.add_plugins((CommandPlugin, CapturePlugin, ViewCommandsPlugin));
            }
            let camera = app.world_mut().spawn(EditorCamera).id();
            assert_eq!(
                CaptureSources::resolve(app.world_mut(), "editor_camera"),
                Ok(CaptureSource::Camera(camera))
            );
            let sources = app.world().resource::<CaptureSources>();
            assert_eq!(sources.names(), ["editor_camera", "game_camera", "window"]);
            for (source, view) in [
                ("editor_camera", "view=scene"),
                ("game_camera", "view=game"),
            ] {
                let hint = sources.error(source, &Inactive).hint.unwrap();
                assert!(hint.contains(view), "{editor_first} {source}: {hint}");
            }
        }
    }

    struct Inactive;

    impl std::fmt::Display for Inactive {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("dark")
        }
    }

    impl ename_engine::capture::CaptureError for Inactive {
        fn kind(&self) -> &'static str {
            "camera_inactive"
        }

        fn is_invalid(&self) -> bool {
            false
        }
    }
}
