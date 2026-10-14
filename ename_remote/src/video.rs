//! `game.video.record` / `.status` / `.stop`: record the window or the game camera to a video.
//!
//! The recording is `ename_engine::video`; this is only its BRP surface. A recording runs for
//! many frames and an instant BRP method must answer in the frame it runs, so `record` spawns a
//! request and returns its entity, `status` reads it, and `stop` ends it early. The editor camera
//! is recorded with `ename_editor`'s `editor.video.record`; `status` and `stop` serve both.

use bevy::{
    prelude::*,
    remote::{BrpError, BrpResult, builtin_methods::parse_some, error_codes},
};
use ename_engine::video::{
    CaptureSource, RetainVideoOutcome, VideoEnd, VideoError, VideoLength, VideoOutcome, VideoPhase,
    VideoProgress, VideoRequest, VideoStopRequested, check_frame_rate, check_video_path,
    parse_timecode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const RECORD_METHOD: &str = "game.video.record";
pub const STATUS_METHOD: &str = "game.video.status";
pub const STOP_METHOD: &str = "game.video.stop";

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Source {
    Window,
    MainCamera,
}

/// A length on the wire. One key, so the unit is never guessed from how a number is written.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Duration {
    Frames(u32),
    Seconds(f64),
    Timecode(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordParams {
    path: PathBuf,
    source: Source,
    duration: Duration,
    frame_rate: Option<u32>,
}

#[derive(Serialize)]
struct RecordResponse {
    request: Entity,
}

#[derive(Deserialize)]
struct RequestParams {
    request: Entity,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum StatusResponse {
    Starting(Progress),
    Recording(Progress),
    Finalizing(Progress),
    Saved {
        path: PathBuf,
        width: u32,
        height: u32,
        frames: u32,
        frame_rate: u32,
        duration_seconds: f64,
        dropped_frames: u32,
        encoder: String,
        ended: &'static str,
    },
}

#[derive(Serialize)]
struct Progress {
    frames: u32,
    dropped_frames: u32,
    elapsed_seconds: f64,
}

pub(crate) fn record(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let RecordParams {
        path,
        source,
        duration,
        frame_rate,
    } = parse_some(params)?;
    let source = match source {
        Source::Window => CaptureSource::PrimaryWindow,
        Source::MainCamera => CaptureSource::Camera(crate::main_camera(world)?),
    };
    let request = video_request(source, path, duration, frame_rate)?;
    let request = world.spawn((request, RetainVideoOutcome)).id();
    crate::to_value(RecordResponse { request })
}

/// Checks what can be checked before the request starts. The editor's method has the same rules.
fn video_request(
    source: CaptureSource,
    path: PathBuf,
    duration: Duration,
    frame_rate: Option<u32>,
) -> Result<VideoRequest, BrpError> {
    let invalid = |err: VideoError| BrpError {
        code: error_codes::INVALID_PARAMS,
        message: err.to_string(),
        data: None,
    };
    check_path(&path)?;
    let length = match duration {
        Duration::Frames(frames) => VideoLength::from_frames(frames),
        Duration::Seconds(seconds) => VideoLength::from_seconds(seconds),
        Duration::Timecode(text) => parse_timecode(&text).map(VideoLength::Time),
    }
    .map_err(invalid)?;
    let mut request = VideoRequest::new(source, path, length);
    if let Some(frame_rate) = frame_rate {
        request.frame_rate = check_frame_rate(frame_rate).map_err(invalid)?;
    }
    Ok(request)
}

pub(crate) fn status(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let RequestParams { request } = parse_some(params)?;
    read_status(world, request)
}

pub(crate) fn stop(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let RequestParams { request } = parse_some(params)?;
    // Check first, so an unknown id is an error rather than a stop sent to nothing.
    read_status(world, request)?;
    world.trigger(VideoStopRequested { entity: request });
    world.flush();
    read_status(world, request)
}

fn read_status(world: &World, request: Entity) -> BrpResult {
    let Some(entity) = world
        .get_entity(request)
        .ok()
        .filter(|entity| entity.contains::<VideoRequest>())
    else {
        return Err(BrpError {
            code: error_codes::INVALID_PARAMS,
            message: format!(
                "{} is not a video request, or it finished more than 5 minutes ago",
                request.to_bits()
            ),
            data: None,
        });
    };
    if let Some(VideoOutcome(outcome)) = entity.get::<VideoOutcome>() {
        return match outcome {
            Ok(saved) => crate::to_value(StatusResponse::Saved {
                path: saved.path.clone(),
                width: saved.size.x,
                height: saved.size.y,
                frames: saved.frames,
                frame_rate: saved.frame_rate,
                duration_seconds: seconds(saved.duration),
                dropped_frames: saved.dropped_frames,
                encoder: saved.encoder.clone(),
                ended: end_name(saved.ended),
            }),
            Err(err) => Err(BrpError {
                code: error_codes::INTERNAL_ERROR,
                message: err.to_string(),
                data: Some(json!({ "kind": error_kind(err) })),
            }),
        };
    }
    let progress = entity.get::<VideoProgress>().cloned().unwrap_or_default();
    let body = Progress {
        frames: progress.frames,
        dropped_frames: progress.dropped_frames,
        elapsed_seconds: seconds(progress.elapsed),
    };
    crate::to_value(match progress.phase {
        VideoPhase::Starting => StatusResponse::Starting(body),
        VideoPhase::Recording => StatusResponse::Recording(body),
        // Done without an outcome only lasts until the outcome's insert is applied.
        VideoPhase::Finalizing | VideoPhase::Done => StatusResponse::Finalizing(body),
    })
}

/// Seconds rounded to the millisecond, so 115 frames at 60 fps read as 1.917 rather than float
/// noise.
fn seconds(duration: std::time::Duration) -> f64 {
    (duration.as_secs_f64() * 1000.0).round() / 1000.0
}

pub(crate) fn end_name(end: VideoEnd) -> &'static str {
    match end {
        VideoEnd::Completed => "completed",
        VideoEnd::Stopped => "stopped",
        VideoEnd::SourceResized => "source_resized",
        VideoEnd::CameraInactive => "camera_inactive",
        VideoEnd::SourceLost => "source_lost",
        VideoEnd::Stalled => "stalled",
    }
}

/// A stable name for each failure, in the error's `data`.
fn error_kind(error: &VideoError) -> &'static str {
    match error {
        VideoError::UnsupportedContainer(_) => "unsupported_container",
        VideoError::InvalidDuration(_) => "invalid_duration",
        VideoError::InvalidFrameRate(_) => "invalid_frame_rate",
        VideoError::NotACamera(_) => "not_a_camera",
        VideoError::CameraInactive(_) => "camera_inactive",
        VideoError::UnsupportedTarget(_) => "unsupported_target",
        VideoError::NoWindow => "no_window",
        VideoError::UnsupportedFormat(_) => "unsupported_format",
        VideoError::EmptyCrop { .. } => "empty_crop",
        VideoError::TooSmall { .. } => "too_small",
        VideoError::TimedOut(_) => "timed_out",
        VideoError::NoFrames(_) => "no_frames",
        VideoError::FfmpegMissing => "ffmpeg_missing",
        VideoError::NoEncoder { .. } => "no_encoder",
        VideoError::Ffmpeg { .. } => "ffmpeg",
        VideoError::Convert(_) => "convert",
        VideoError::Write { .. } => "write",
    }
}

/// Relative paths are refused: the game's working directory is not the caller's.
fn check_path(path: &Path) -> Result<(), BrpError> {
    let message = if path.is_absolute() {
        match check_video_path(path) {
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
    use ename_engine::{camera::MainCamera, video::SavedVideo};
    use std::{num::NonZeroU32, time::Duration as StdDuration};

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

    fn recorded(world: &mut World, params: Value) -> VideoRequest {
        let response = call(world, record, params).unwrap();
        let entity = world.entity(request_of(&response));
        assert!(entity.contains::<RetainVideoOutcome>());
        entity.get::<VideoRequest>().unwrap().clone()
    }

    #[test]
    fn record_reads_each_duration_form() {
        let mut world = World::new();
        let cases = [
            (
                json!({ "frames": 600 }),
                VideoLength::from_frames(600).unwrap(),
            ),
            (
                json!({ "seconds": 10 }),
                VideoLength::Time(StdDuration::from_secs(10)),
            ),
            (
                json!({ "seconds": 2.5 }),
                VideoLength::Time(StdDuration::from_millis(2500)),
            ),
            (
                json!({ "timecode": "00:01:30" }),
                VideoLength::Time(StdDuration::from_secs(90)),
            ),
        ];
        for (duration, length) in cases {
            let request = recorded(
                &mut world,
                json!({ "path": "/tmp/a.mp4", "source": "window", "duration": duration }),
            );
            assert_eq!(request.length, length);
            assert_eq!(request.source, CaptureSource::PrimaryWindow);
            assert_eq!(request.frame_rate.get(), 60);
        }
        let request = recorded(
            &mut world,
            json!({ "path": "/tmp/a.mkv", "source": "window", "duration": { "frames": 1 }, "frame_rate": 30 }),
        );
        assert_eq!(request.frame_rate, NonZeroU32::new(30).unwrap());
    }

    #[test]
    fn record_rejects_bad_params() {
        let mut world = World::new();
        let base = |path: &str, duration: Value| json!({ "path": path, "source": "window", "duration": duration });
        for params in [
            base("a.mp4", json!({ "frames": 1 })),
            base("/tmp/a.webm", json!({ "frames": 1 })),
            base("/tmp/a.mp4", json!({ "frames": 0 })),
            base("/tmp/a.mp4", json!({ "seconds": -1 })),
            base("/tmp/a.mp4", json!({ "timecode": "x" })),
            base("/tmp/a.mp4", json!({ "frames": 1, "seconds": 1 })),
            base("/tmp/a.mp4", json!(10)),
            base("/tmp/a.mp4", json!("00:00:10")),
            json!({ "path": "/tmp/a.mp4", "source": "window", "duration": { "frames": 1 }, "frame_rate": 0 }),
            json!({ "path": "/tmp/a.mp4", "source": "window", "duration": { "frames": 1 }, "frame_rate": 241 }),
        ] {
            let err = call(&mut world, record, params.clone()).unwrap_err();
            assert_eq!(err.code, error_codes::INVALID_PARAMS, "{params}");
        }
        assert_eq!(world.query::<&VideoRequest>().iter(&world).count(), 0);
    }

    #[test]
    fn record_main_camera_uses_the_one_main_camera() {
        let mut world = World::new();
        let params =
            json!({ "path": "/tmp/a.mp4", "source": "main_camera", "duration": { "frames": 1 } });
        let err = call(&mut world, record, params.clone()).unwrap_err();
        assert!(err.message.contains("no MainCamera"), "{}", err.message);
        let camera = world
            .spawn((MainCamera, Camera::default(), RenderTarget::default()))
            .id();
        assert_eq!(
            recorded(&mut world, params).source,
            CaptureSource::Camera(camera)
        );
    }

    fn spawn_request(world: &mut World) -> Entity {
        world
            .spawn(VideoRequest::new(
                CaptureSource::PrimaryWindow,
                "/tmp/a.mp4",
                VideoLength::from_frames(1).unwrap(),
            ))
            .id()
    }

    #[test]
    fn status_reports_each_phase_and_outcome() {
        let mut world = World::new();
        let entity = spawn_request(&mut world);
        assert_eq!(
            call(&mut world, status, json!({ "request": entity })).unwrap(),
            json!({ "status": "starting", "frames": 0, "dropped_frames": 0, "elapsed_seconds": 0.0 })
        );
        *world.get_mut::<VideoProgress>(entity).unwrap() = VideoProgress {
            phase: VideoPhase::Recording,
            frames: 12,
            dropped_frames: 1,
            elapsed: StdDuration::from_millis(200),
        };
        assert_eq!(
            call(&mut world, status, json!({ "request": entity })).unwrap(),
            json!({ "status": "recording", "frames": 12, "dropped_frames": 1, "elapsed_seconds": 0.2 })
        );
        world.entity_mut(entity).insert(VideoOutcome(Ok(SavedVideo {
            path: "/tmp/a.mp4".into(),
            size: UVec2::new(640, 360),
            frames: 600,
            frame_rate: 60,
            duration: StdDuration::from_secs_f64(115.0 / 60.0),
            dropped_frames: 2,
            encoder: "h264_vaapi".into(),
            ended: VideoEnd::Stopped,
        })));
        assert_eq!(
            call(&mut world, status, json!({ "request": entity })).unwrap(),
            json!({
                "status": "saved", "path": "/tmp/a.mp4", "width": 640, "height": 360,
                "frames": 600, "frame_rate": 60, "duration_seconds": 1.917, "dropped_frames": 2,
                "encoder": "h264_vaapi", "ended": "stopped",
            })
        );
        world
            .entity_mut(entity)
            .insert(VideoOutcome(Err(VideoError::FfmpegMissing)));
        let err = call(&mut world, status, json!({ "request": entity })).unwrap_err();
        assert_eq!(err.data, Some(json!({ "kind": "ffmpeg_missing" })));
    }

    #[test]
    fn stop_triggers_the_stop_and_unknown_ids_are_rejected() {
        let mut world = World::new();
        let entity = spawn_request(&mut world);
        world.init_resource::<Stops>();
        world.add_observer(|stop: On<VideoStopRequested>, mut stops: ResMut<Stops>| {
            stops.0.push(stop.entity);
        });
        call(&mut world, stop, json!({ "request": entity })).unwrap();
        assert_eq!(world.resource::<Stops>().0, [entity]);

        let other = world.spawn_empty().id();
        let err = call(&mut world, stop, json!({ "request": other })).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert_eq!(world.resource::<Stops>().0, [entity]);
    }

    #[derive(Resource, Default)]
    struct Stops(Vec<Entity>);
}
