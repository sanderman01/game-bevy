//! `editor.view.get` / `.set`: which of Scene View and Game View is shown, over BRP. And
//! `editor.screenshot.capture`: the Scene View's camera to a PNG.
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
    RetainScreenshotOutcome, ScreenshotRequest, ScreenshotSource, check_png_path,
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
        let mut methods = world.resource_mut::<RemoteMethods>();
        methods.insert(GET_METHOD, RemoteMethodSystemId::Instant(get));
        methods.insert(SET_METHOD, RemoteMethodSystemId::Instant(set));
        methods.insert(SCREENSHOT_METHOD, RemoteMethodSystemId::Instant(screenshot));
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
    let camera = world
        .query_filtered::<Entity, With<EditorCamera>>()
        .single(world)
        .map_err(|err| BrpError {
            code: error_codes::INTERNAL_ERROR,
            message: format!("the editor camera is missing: {err}"),
            data: None,
        })?;
    let request = world
        .spawn((
            ScreenshotRequest {
                source: ScreenshotSource::Camera(camera),
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
            ScreenshotSource::Camera(camera)
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
