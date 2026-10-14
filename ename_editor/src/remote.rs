//! `editor.view.get` / `.set`: which of Scene View and Game View is shown, over BRP. And
//! `editor.screenshot.capture`: the Scene View's camera to a PNG. And, with the `video` feature,
//! `editor.video.record`: the Scene View's camera to a video.
//!
//! The editor registers these itself rather than `ename_remote` doing it, because the dock and
//! camera they use are private to this crate and neither crate may depend on the other. They go
//! into Bevy's own `RemoteMethods`, so this module needs `bevy_remote` and no `ename_remote`.
//!
//! # Timing
//!
//! BRP handlers run in `RemoteLast`, after `Last`. A successful `set` means the selection is
//! applied: the dock's active tab and focus, and `UiState::active_viewport`, so a `get` straight
//! after reports it. The cameras and layout follow on the next frame's egui pass, the same pass
//! that follows a click on the tab.

use bevy::{
    prelude::*,
    remote::{
        BrpError, BrpResult, RemoteMethodSystemId, RemoteMethods, builtin_methods::parse_some,
        error_codes,
    },
};
use ename_engine::screenshot::{
    CaptureSource, RetainScreenshotOutcome, ScreenshotRequest, check_png_path,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::{
    camera::EditorCamera,
    panels::{ActiveViewport, UiState},
};

pub const GET_METHOD: &str = "editor.view.get";
pub const SET_METHOD: &str = "editor.view.set";
/// The outcome is read with `ename_remote`'s `game.screenshot.result`.
pub const SCREENSHOT_METHOD: &str = "editor.screenshot.capture";
/// Status and stop are `ename_remote`'s `game.video.status` / `.stop`.
#[cfg(feature = "video")]
pub const VIDEO_METHOD: &str = "editor.video.record";

/// Adds the `editor.view.*` methods to the BRP server, when the target has one.
pub(crate) struct RemoteViewPlugin;

impl Plugin for RemoteViewPlugin {
    fn build(&self, _app: &mut App) {}

    /// `finish`, not `build`: `RemotePlugin` inserts `RemoteMethods` while it builds, and it may be
    /// added after the editor. A target without a BRP server gets nothing.
    fn finish(&self, app: &mut App) {
        let world = app.world_mut();
        if !world.contains_resource::<RemoteMethods>() {
            return;
        }
        let get = world.register_system(get);
        let set = world.register_system(set);
        let screenshot = world.register_system(capture_screenshot);
        #[cfg(feature = "video")]
        let video = world.register_system(video::record);
        let mut methods = world.resource_mut::<RemoteMethods>();
        methods.insert(GET_METHOD, RemoteMethodSystemId::Instant(get));
        methods.insert(SET_METHOD, RemoteMethodSystemId::Instant(set));
        methods.insert(SCREENSHOT_METHOD, RemoteMethodSystemId::Instant(screenshot));
        #[cfg(feature = "video")]
        methods.insert(VIDEO_METHOD, RemoteMethodSystemId::Instant(video));
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Serialize)]
struct ViewResponse {
    view: View,
    /// Whether the Game View has a camera to show. False makes it show a placeholder instead.
    game_camera: bool,
}

#[derive(Deserialize)]
struct SetParams {
    view: View,
}

fn get(In(_): In<Option<Value>>, world: &mut World) -> BrpResult {
    read(world)
}

fn set(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let SetParams { view } = parse_some(params)?;
    world
        .resource_mut::<UiState>()
        .show_viewport(view.into())
        .map_err(|message| BrpError {
            code: error_codes::INTERNAL_ERROR,
            message,
            data: None,
        })?;
    read(world)
}

#[derive(Deserialize)]
struct ScreenshotParams {
    path: PathBuf,
}

#[derive(Serialize)]
struct ScreenshotResponse {
    request: Entity,
}

fn capture_screenshot(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let ScreenshotParams { path } = parse_some(params)?;
    check_screenshot_path(&path)?;
    let camera = editor_camera(world)?;
    let request = world
        .spawn((
            ScreenshotRequest {
                source: CaptureSource::Camera(camera),
                path,
            },
            RetainScreenshotOutcome,
        ))
        .id();
    serde_json::to_value(ScreenshotResponse { request }).map_err(|err| BrpError {
        code: error_codes::INTERNAL_ERROR,
        message: err.to_string(),
        data: None,
    })
}

fn editor_camera(world: &mut World) -> Result<Entity, BrpError> {
    world
        .query_filtered::<Entity, With<EditorCamera>>()
        .single(world)
        .map_err(|err| BrpError {
            code: error_codes::INTERNAL_ERROR,
            message: format!("the editor camera is missing: {err}"),
            data: None,
        })
}

/// `editor.video.record`, with the same params and rules as `game.video.record` minus `source`.
#[cfg(feature = "video")]
mod video {
    use super::*;
    use ename_engine::video::{
        CaptureSource, RetainVideoOutcome, VideoError, VideoLength, VideoRequest, check_frame_rate,
        check_video_path, parse_timecode,
    };

    /// One key, so the unit is never guessed from how a number is written.
    #[derive(Deserialize)]
    #[serde(rename_all = "snake_case", deny_unknown_fields)]
    enum Duration {
        Frames(u32),
        Seconds(f64),
        Timecode(String),
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        path: PathBuf,
        duration: Duration,
        frame_rate: Option<u32>,
    }

    pub(super) fn record(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
        let Params {
            path,
            duration,
            frame_rate,
        } = parse_some(params)?;
        let invalid = |message: String| BrpError {
            code: error_codes::INVALID_PARAMS,
            message,
            data: None,
        };
        if !path.is_absolute() {
            return Err(invalid(format!(
                "`{}` is not an absolute path",
                path.display()
            )));
        }
        let checked = (|| -> Result<VideoRequest, VideoError> {
            check_video_path(&path)?;
            let length = match duration {
                Duration::Frames(frames) => VideoLength::from_frames(frames)?,
                Duration::Seconds(seconds) => VideoLength::from_seconds(seconds)?,
                Duration::Timecode(text) => VideoLength::Time(parse_timecode(&text)?),
            };
            let mut request = VideoRequest::new(CaptureSource::PrimaryWindow, path, length);
            if let Some(frame_rate) = frame_rate {
                request.frame_rate = check_frame_rate(frame_rate)?;
            }
            Ok(request)
        })();
        let mut request = checked.map_err(|err| invalid(err.to_string()))?;
        request.source = CaptureSource::Camera(editor_camera(world)?);
        let request = world.spawn((request, RetainVideoOutcome)).id();
        serde_json::to_value(ScreenshotResponse { request }).map_err(|err| BrpError {
            code: error_codes::INTERNAL_ERROR,
            message: err.to_string(),
            data: None,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;

        #[test]
        fn records_the_editor_camera() {
            let mut world = World::new();
            let camera = world.spawn(EditorCamera).id();
            let response = world
                .run_system_cached_with(
                    record,
                    Some(
                        json!({ "path": "/tmp/scene.mp4", "duration": { "timecode": "00:00:03" } }),
                    ),
                )
                .unwrap()
                .unwrap();
            let request: Entity = serde_json::from_value(response["request"].clone()).unwrap();
            let request = world.entity(request);
            let video = request.get::<VideoRequest>().unwrap();
            assert_eq!(video.source, CaptureSource::Camera(camera));
            assert_eq!(
                video.length,
                VideoLength::Time(std::time::Duration::from_secs(3))
            );
            assert!(request.contains::<RetainVideoOutcome>());
        }

        #[test]
        fn rejects_bad_params_and_a_missing_camera() {
            let mut world = World::new();
            for params in [
                json!({ "path": "scene.mp4", "duration": { "frames": 1 } }),
                json!({ "path": "/tmp/scene.png", "duration": { "frames": 1 } }),
                json!({ "path": "/tmp/scene.mp4", "duration": 10 }),
            ] {
                let err = world
                    .run_system_cached_with(record, Some(params.clone()))
                    .unwrap()
                    .unwrap_err();
                assert_eq!(err.code, error_codes::INVALID_PARAMS, "{params}");
            }
            let err = world
                .run_system_cached_with(
                    record,
                    Some(json!({ "path": "/tmp/scene.mp4", "duration": { "frames": 1 } })),
                )
                .unwrap()
                .unwrap_err();
            assert!(err.message.contains("editor camera"), "{}", err.message);
        }
    }
}

/// The same rule as `game.screenshot.capture`: absolute, because the game's working directory is
/// not the caller's, and `.png`.
fn check_screenshot_path(path: &Path) -> Result<(), BrpError> {
    let message = if path.is_absolute() {
        match check_png_path(path) {
            Ok(()) => return Ok(()),
            Err(err) => err.to_string(),
        }
    } else {
        format!("`{}` is not an absolute path", path.display())
    };
    Err(BrpError {
        code: error_codes::INVALID_PARAMS,
        message,
        data: None,
    })
}

fn read(world: &mut World) -> BrpResult {
    let response = ViewResponse {
        view: world.resource::<UiState>().active_viewport.into(),
        game_camera: crate::game_view::camera_available(world),
    };
    serde_json::to_value(response).map_err(|err| BrpError {
        code: error_codes::INTERNAL_ERROR,
        message: err.to_string(),
        data: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{camera::RenderTarget, remote::RemotePlugin};
    use ename_engine::camera::MainCamera;
    use serde_json::json;

    fn app_with_remote(remote_first: bool) -> App {
        let mut app = App::new();
        if remote_first {
            app.add_plugins((RemotePlugin::default(), RemoteViewPlugin));
        } else {
            app.add_plugins((RemoteViewPlugin, RemotePlugin::default()));
        }
        app.finish();
        app
    }

    #[test]
    fn registers_whichever_order_the_plugins_are_added_in() {
        for remote_first in [true, false] {
            let app = app_with_remote(remote_first);
            let methods = app.world().resource::<RemoteMethods>();
            assert!(methods.get(GET_METHOD).is_some(), "{remote_first}");
            assert!(methods.get(SET_METHOD).is_some(), "{remote_first}");
            assert!(methods.get(SCREENSHOT_METHOD).is_some(), "{remote_first}");
            #[cfg(feature = "video")]
            assert!(methods.get(VIDEO_METHOD).is_some(), "{remote_first}");
        }
    }

    #[test]
    fn does_nothing_without_a_remote_server() {
        let mut app = App::new();
        app.add_plugins(RemoteViewPlugin);
        app.finish();
        assert!(!app.world().contains_resource::<RemoteMethods>());
    }

    fn world() -> World {
        let mut world = World::new();
        world.insert_resource(UiState::new());
        world
    }

    fn call<M>(
        world: &mut World,
        method: impl IntoSystem<In<Option<Value>>, BrpResult, M> + 'static,
        params: Option<Value>,
    ) -> BrpResult {
        world.run_system_cached_with(method, params).unwrap()
    }

    #[test]
    fn set_shows_the_view_and_get_reads_it_back() {
        let mut world = world();
        let shown = call(&mut world, set, Some(json!({ "view": "game" }))).unwrap();
        assert_eq!(shown, json!({ "view": "game", "game_camera": false }));
        assert_eq!(
            call(&mut world, get, None).unwrap(),
            json!({ "view": "game", "game_camera": false })
        );
        assert_eq!(
            world.resource::<UiState>().active_viewport,
            ActiveViewport::Game
        );
    }

    #[test]
    fn an_unknown_view_is_invalid_params() {
        let mut world = world();
        let err = call(&mut world, set, Some(json!({ "view": "top" }))).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn game_camera_means_exactly_one_camera_the_game_view_can_use() {
        let game_camera =
            |world: &mut World| call(world, get, None).unwrap()["game_camera"].clone();
        let mut world = world();
        assert_eq!(game_camera(&mut world), json!(false));

        // A marker on something that is not a camera is not a camera.
        world.spawn(MainCamera);
        assert_eq!(game_camera(&mut world), json!(false));

        world.spawn((MainCamera, Camera::default(), RenderTarget::default()));
        assert_eq!(game_camera(&mut world), json!(true));

        // Two, which the Game View treats as none.
        world.spawn((MainCamera, Camera::default(), RenderTarget::default()));
        assert_eq!(game_camera(&mut world), json!(false));
    }

    #[test]
    fn screenshot_captures_the_editor_camera() {
        let mut world = world();
        let camera = world.spawn(EditorCamera).id();
        let response = call(
            &mut world,
            capture_screenshot,
            Some(json!({ "path": "/tmp/scene.png" })),
        )
        .unwrap();
        let request: Entity = serde_json::from_value(response["request"].clone()).unwrap();
        let request = world.entity(request);
        assert_eq!(
            request.get::<ScreenshotRequest>().unwrap().source,
            CaptureSource::Camera(camera)
        );
        assert!(request.contains::<RetainScreenshotOutcome>());
    }

    #[test]
    fn screenshot_rejects_relative_and_non_png_paths() {
        let mut world = world();
        world.spawn(EditorCamera);
        for path in ["scene.png", "/tmp/scene.jpg"] {
            let err = call(
                &mut world,
                capture_screenshot,
                Some(json!({ "path": path })),
            )
            .unwrap_err();
            assert_eq!(err.code, error_codes::INVALID_PARAMS, "{path}");
        }
    }

    #[test]
    fn screenshot_without_an_editor_camera_is_an_error() {
        let mut world = world();
        let err = call(
            &mut world,
            capture_screenshot,
            Some(json!({ "path": "/tmp/scene.png" })),
        )
        .unwrap_err();
        assert!(err.message.contains("editor camera"), "{}", err.message);
        let requests = world.query::<&ScreenshotRequest>().iter(&world).count();
        assert_eq!(requests, 0);
    }
}
