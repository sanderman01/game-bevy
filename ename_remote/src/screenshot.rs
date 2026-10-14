//! `game.screenshot.capture` / `.result`: capture the window or the game camera to a PNG.
//!
//! The capture is `ename_engine::screenshot`; this is only its BRP surface. It is two calls
//! because the GPU readback completes frames after the request, and an instant BRP method must
//! answer in the frame it runs. `capture` spawns a request and returns its entity; `result` reads
//! it, and keeps reading the same answer until the engine expires it.
//!
//! The editor camera is not here. `ename_editor` registers `editor.screenshot.capture` itself,
//! because its camera marker is private to it. `result` serves both, since the outcome lives on
//! an engine component.

use bevy::{
    prelude::*,
    remote::{BrpError, BrpResult, builtin_methods::parse_some, error_codes},
};
use ename_engine::screenshot::{
    CaptureSource, RetainScreenshotOutcome, ScreenshotError, ScreenshotOutcome, ScreenshotRequest,
    check_png_path,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const CAPTURE_METHOD: &str = "game.screenshot.capture";
pub const RESULT_METHOD: &str = "game.screenshot.result";

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Source {
    Window,
    MainCamera,
}

#[derive(Deserialize)]
struct CaptureParams {
    path: PathBuf,
    source: Source,
}

#[derive(Serialize)]
struct CaptureResponse {
    request: Entity,
}

#[derive(Deserialize)]
struct ResultParams {
    request: Entity,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ResultResponse {
    Pending,
    Saved {
        path: PathBuf,
        width: u32,
        height: u32,
    },
}

pub(crate) fn capture(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let CaptureParams { path, source } = parse_some(params)?;
    check_path(&path)?;
    let source = match source {
        Source::Window => CaptureSource::PrimaryWindow,
        Source::MainCamera => CaptureSource::Camera(crate::main_camera(world)?),
    };
    let request = world
        .spawn((ScreenshotRequest { source, path }, RetainScreenshotOutcome))
        .id();
    crate::to_value(CaptureResponse { request })
}

pub(crate) fn result(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let ResultParams { request } = parse_some(params)?;
    let Some(entity) = world
        .get_entity(request)
        .ok()
        .filter(|entity| entity.contains::<ScreenshotRequest>())
    else {
        return Err(BrpError {
            code: error_codes::INVALID_PARAMS,
            message: format!(
                "{} is not a screenshot request, or it finished more than 30 s ago",
                request.to_bits()
            ),
            data: None,
        });
    };
    match entity.get::<ScreenshotOutcome>() {
        None => crate::to_value(ResultResponse::Pending),
        Some(ScreenshotOutcome(Ok(saved))) => crate::to_value(ResultResponse::Saved {
            path: saved.path.clone(),
            width: saved.size.x,
            height: saved.size.y,
        }),
        Some(ScreenshotOutcome(Err(err))) => Err(BrpError {
            code: error_codes::INTERNAL_ERROR,
            message: err.to_string(),
            data: Some(json!({ "kind": error_kind(err) })),
        }),
    }
}

/// A stable name for each failure, in the error's `data`, so a client can react to one without
/// matching on the message.
fn error_kind(error: &ScreenshotError) -> &'static str {
    match error {
        ScreenshotError::NotPng(_) => "not_png",
        ScreenshotError::NotACamera(_) => "not_a_camera",
        ScreenshotError::CameraInactive(_) => "camera_inactive",
        ScreenshotError::UnsupportedTarget(_) => "unsupported_target",
        ScreenshotError::NoWindow => "no_window",
        ScreenshotError::UnsupportedFormat(_) => "unsupported_format",
        ScreenshotError::EmptyCrop { .. } => "empty_crop",
        ScreenshotError::TimedOut(_) => "timed_out",
        ScreenshotError::Convert(_) => "convert",
        ScreenshotError::Write { .. } => "write",
    }
}

/// Relative paths are refused: the game's working directory is not the caller's, and a file
/// landing somewhere unexpected is worse than an error.
fn check_path(path: &Path) -> Result<(), BrpError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    use bevy::camera::RenderTarget;
    use ename_engine::{camera::MainCamera, screenshot::SavedScreenshot};
    use serde_json::json;

    fn call<M>(
        world: &mut World,
        method: impl IntoSystem<In<Option<Value>>, BrpResult, M> + 'static,
        params: Value,
    ) -> BrpResult {
        world.run_system_cached_with(method, Some(params)).unwrap()
    }

    fn request_of(response: &Value) -> Entity {
        serde_json::from_value(response["request"].clone()).unwrap()
    }

    #[test]
    fn capture_window_spawns_a_retained_request() {
        let mut world = World::new();
        let response = call(
            &mut world,
            capture,
            json!({ "path": "/tmp/a.png", "source": "window" }),
        )
        .unwrap();
        let entity = world.entity(request_of(&response));
        assert_eq!(
            entity.get::<ScreenshotRequest>().unwrap().source,
            CaptureSource::PrimaryWindow
        );
        assert!(entity.contains::<RetainScreenshotOutcome>());
    }

    #[test]
    fn capture_rejects_relative_and_non_png_paths() {
        let mut world = World::new();
        for path in ["a.png", "/tmp/a.jpg"] {
            let err = call(
                &mut world,
                capture,
                json!({ "path": path, "source": "window" }),
            )
            .unwrap_err();
            assert_eq!(err.code, error_codes::INVALID_PARAMS, "{path}");
        }
        let requests = world.query::<&ScreenshotRequest>().iter(&world).count();
        assert_eq!(requests, 0);
    }

    #[test]
    fn main_camera_needs_exactly_one() {
        let mut world = World::new();
        let params = json!({ "path": "/tmp/a.png", "source": "main_camera" });
        let err = call(&mut world, capture, params.clone()).unwrap_err();
        assert!(err.message.contains("no MainCamera"), "{}", err.message);

        let camera = world
            .spawn((MainCamera, Camera::default(), RenderTarget::default()))
            .id();
        let response = call(&mut world, capture, params.clone()).unwrap();
        assert_eq!(
            world
                .get::<ScreenshotRequest>(request_of(&response))
                .unwrap()
                .source,
            CaptureSource::Camera(camera)
        );

        world.spawn((MainCamera, Camera::default(), RenderTarget::default()));
        let err = call(&mut world, capture, params).unwrap_err();
        assert!(err.message.contains("ambiguous"), "{}", err.message);
    }

    #[test]
    fn result_reports_pending_saved_and_failed() {
        let mut world = World::new();
        let request = ScreenshotRequest {
            source: CaptureSource::PrimaryWindow,
            path: "/tmp/a.png".into(),
        };
        let pending = world.spawn(request.clone()).id();
        let saved = world
            .spawn((
                request.clone(),
                ScreenshotOutcome(Ok(SavedScreenshot {
                    path: "/tmp/a.png".into(),
                    size: UVec2::new(4, 3),
                })),
            ))
            .id();
        let failed = world
            .spawn((request, ScreenshotOutcome(Err(ScreenshotError::NoWindow))))
            .id();

        assert_eq!(
            call(&mut world, result, json!({ "request": pending })).unwrap(),
            json!({ "status": "pending" })
        );
        let expected = json!({ "status": "saved", "path": "/tmp/a.png", "width": 4, "height": 3 });
        // Repeatable: a caller whose response was lost can ask again.
        for _ in 0..2 {
            assert_eq!(
                call(&mut world, result, json!({ "request": saved })).unwrap(),
                expected
            );
        }
        let err = call(&mut world, result, json!({ "request": failed })).unwrap_err();
        assert_eq!(err.message, ScreenshotError::NoWindow.to_string());
        assert_eq!(err.data, Some(json!({ "kind": "no_window" })));
    }

    #[test]
    fn result_rejects_what_is_not_a_live_request() {
        let mut world = World::new();
        let other = world.spawn_empty().id();
        let gone = world.spawn_empty().id();
        world.despawn(gone);
        for request in [other, gone] {
            let err = call(&mut world, result, json!({ "request": request })).unwrap_err();
            assert_eq!(err.code, error_codes::INVALID_PARAMS);
            assert!(
                err.message.starts_with(&request.to_bits().to_string()),
                "{}",
                err.message
            );
        }
    }
}
