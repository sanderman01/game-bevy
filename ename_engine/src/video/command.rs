//! `video.record`: a recording as a job command.
//!
//! The job's work entity is the [`VideoRequest`]. Cancelling the job stops the recording and keeps
//! what was recorded; a recording stopped before its first frame has nothing to keep and ends
//! `cancelled`. Progress is copied from [`VideoProgress`] while it records.

use std::path::PathBuf;

use bevy::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    VideoEnd, VideoError, VideoFinished, VideoLength, VideoPhase, VideoProgress, VideoRequest,
    VideoStopRequested, check_frame_rate, check_video_path, parse_timecode,
};
use crate::{
    capture::{CaptureError, CaptureSources, restrict_source},
    command::{
        Call, CommandAppExt, CommandError, CommandSpec, JobCancelRequested, JobHandle, JobWork,
        Jobs, OnCancel,
    },
};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RecordParams {
    /// Where to write the video: `.mp4` or `.mkv`, H.264. A relative path resolves against the
    /// caller's working directory. Missing directories are created and an existing file is
    /// replaced.
    path: PathBuf,
    /// What to record. "window" is the whole game window. "game_camera" is the stage's
    /// MainCamera (in the editor, the Game View). Plugins add more, such as the editor's
    /// "editor_camera". A camera source must stay visible for the whole recording.
    source: String,
    /// How long to record. Exactly one key: {"frames": 600} records 600 rendered frames;
    /// {"seconds": 10} records 10 s of real time; {"timecode": "00:01:30"} is hh:mm:ss. At most
    /// 24 hours.
    duration: Duration,
    /// Output frames per second, 1 to 240. Defaults to 30.
    #[serde(default)]
    frame_rate: Option<u32>,
}

/// A length with its unit as the key, so `10` is never read as frames when seconds were meant.
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Duration {
    Frames(u32),
    Seconds(f64),
    Timecode(String),
}

#[derive(Serialize, JsonSchema)]
struct Recorded {
    /// Absolute.
    path: PathBuf,
    /// Physical pixels.
    width: u32,
    height: u32,
    /// Frames in the file.
    frames: u32,
    frame_rate: u32,
    duration_seconds: f64,
    /// Frames lost because the encoder or the GPU readback fell behind.
    dropped_frames: u32,
    /// The ffmpeg encoder used, such as h264_vaapi.
    encoder: String,
    /// Why it ended: "completed", or why it ended early ("stopped", "source_resized",
    /// "camera_inactive", "source_lost", "stalled").
    ended: String,
    /// Present when it ended early.
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Serialize)]
struct Progress {
    phase: &'static str,
    frames: u32,
    dropped_frames: u32,
    elapsed_seconds: f64,
}

/// Registers `video.record`. Called from `VideoPlugin::finish`, so every capture source a plugin
/// added in `build` is in the schema.
pub(super) fn register(app: &mut App) {
    let names = app
        .world()
        .get_resource::<CaptureSources>()
        .map(CaptureSources::names)
        .unwrap_or_default();
    app.register_job_command(
        CommandSpec::new(
            "video.record",
            "Record the game window, the game camera or the editor camera to an .mp4 or .mkv \
             file.",
        )
        .adjust_params_schema(restrict_source(names)),
        record,
    );
}

fn record(
    In(call): In<Call<RecordParams>>,
    world: &mut World,
) -> Result<JobHandle<Recorded>, CommandError> {
    let params = &call.params;
    let invalid = |err: VideoError| CommandError::invalid(err.kind(), err.to_string());
    let path = call.resolve_path(&params.path)?;
    check_video_path(&path).map_err(invalid)?;
    let length = match &params.duration {
        Duration::Frames(frames) => VideoLength::from_frames(*frames),
        Duration::Seconds(seconds) => VideoLength::from_seconds(*seconds),
        Duration::Timecode(text) => parse_timecode(text).map(VideoLength::Time),
    }
    .map_err(invalid)?;
    let frame_rate = params
        .frame_rate
        .map(check_frame_rate)
        .transpose()
        .map_err(invalid)?;

    let name = params.source.clone();
    let source = CaptureSources::resolve(world, &name)?;
    let mut request = VideoRequest::new(source, path, length);
    if let Some(frame_rate) = frame_rate {
        request.frame_rate = frame_rate;
    }
    let work = world.spawn(request).id();
    let job = match call.start_job::<Recorded>(world, work, OnCancel::Notify) {
        Ok(job) => job,
        Err(err) => {
            world.despawn(work);
            return Err(err);
        }
    };
    let handle = job.clone();
    world
        .entity_mut(work)
        .observe(
            move |done: On<VideoFinished>, mut jobs: ResMut<Jobs>, sources: Res<CaptureSources>| {
                match &done.result {
                    Ok(saved) => jobs.finish(&handle, Ok(recorded(saved))),
                    // Stopped before anything was recorded because the caller asked: nothing to
                    // keep, and nothing failed.
                    Err(VideoError::NoFrames(VideoEnd::Stopped))
                        if jobs.is_cancel_requested(handle.id()) =>
                    {
                        jobs.cancelled(handle.id());
                    }
                    Err(err) => jobs.finish(&handle, Err(sources.error(&name, err))),
                }
            },
        )
        .observe(|cancel: On<JobCancelRequested>, mut commands: Commands| {
            commands.trigger(VideoStopRequested {
                entity: cancel.entity,
            });
        });
    Ok(job)
}

fn recorded(saved: &super::SavedVideo) -> Recorded {
    let ended = saved.ended.name();
    Recorded {
        path: saved.path.clone(),
        width: saved.size.x,
        height: saved.size.y,
        frames: saved.frames,
        frame_rate: saved.frame_rate,
        duration_seconds: seconds(saved.duration),
        dropped_frames: saved.dropped_frames,
        encoder: saved.encoder.clone(),
        ended: ended.to_owned(),
        note: (saved.ended != VideoEnd::Completed).then(|| {
            format!(
                "ended early ({ended}) after {} frames; the file holds what was recorded",
                saved.frames
            )
        }),
    }
}

/// Copies each recording's progress into its job.
pub(super) fn sync_job_progress(
    jobs: Option<ResMut<Jobs>>,
    recordings: Query<(&JobWork, &VideoProgress), Changed<VideoProgress>>,
) {
    let Some(mut jobs) = jobs else {
        return;
    };
    for (work, progress) in &recordings {
        jobs.set_progress(
            work.job(),
            Progress {
                phase: match progress.phase {
                    VideoPhase::Starting => "starting",
                    VideoPhase::Recording => "recording",
                    VideoPhase::Finalizing | VideoPhase::Done => "finalizing",
                },
                frames: progress.frames,
                dropped_frames: progress.dropped_frames,
                elapsed_seconds: seconds(progress.elapsed),
            },
        );
    }
}

/// Seconds rounded to the millisecond, so 115 frames at 60 fps read as 1.917 rather than float
/// noise.
fn seconds(duration: std::time::Duration) -> f64 {
    (duration.as_secs_f64() * 1000.0).round() / 1000.0
}
