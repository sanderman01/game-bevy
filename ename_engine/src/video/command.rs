//! `video.record`: a recording as a job command.
//!
//! The job's work entity is the [`VideoRequest`]. Cancelling the job stops the recording and keeps
//! what was recorded; a recording stopped before its first frame has nothing to keep and ends
//! `cancelled`. With `settle`, the entity first carries a [`SettleGate`] and gets its request once
//! the gate opens; cancelled while settling, the job ends `cancelled` with no file. Progress is
//! copied from [`VideoProgress`] while it records.

use std::path::PathBuf;

use bevy::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    VideoEnd, VideoError, VideoFinished, VideoLength, VideoPhase, VideoProgress, VideoRequest,
    VideoStopRequested, check_frame_rate, check_video_path,
};
use crate::{
    capture::{CaptureError, CaptureSources, restrict_source},
    command::{
        Call, CommandAppExt, CommandError, CommandSpec, JobCancelRequested, JobHandle, JobWork,
        Jobs, Length, OnCancel,
    },
    wait::{SettleGate, SettleParams, SettleTimedOut, Settled},
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
    duration: Length,
    /// Output frames per second, 1 to 240. Defaults to 30.
    #[serde(default)]
    frame_rate: Option<u32>,
    /// Wait for loading, pipelines and streaming to settle first, as `wait.settled` does; `{}`
    /// uses its defaults. Recording starts once it settles; fails `not_settled` on timeout.
    #[serde(default)]
    settle: Option<SettleParams>,
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
        Length::Frames(frames) => VideoLength::from_frames(*frames),
        Length::Seconds(seconds) => VideoLength::from_seconds(*seconds),
        Length::Timecode(text) => VideoLength::from_timecode(text),
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
    let gate = params
        .settle
        .as_ref()
        .map(|settle| SettleGate::new(settle, world))
        .transpose()?;
    let work = match gate {
        Some(gate) => world.spawn(gate).id(),
        None => world.spawn(request.clone()).id(),
    };
    let job = match call.start_job::<Recorded>(world, work, OnCancel::Notify) {
        Ok(job) => job,
        Err(err) => {
            world.despawn(work);
            return Err(err);
        }
    };
    let handle = job.clone();
    let (timed_out, settling_cancelled) = (job.clone(), job.clone());
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
        )
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
        .observe(
            move |cancel: On<JobCancelRequested>,
                  settling: Query<(), With<SettleGate>>,
                  mut jobs: ResMut<Jobs>,
                  mut commands: Commands| {
                // Still settling: nothing was recorded, so there is nothing to keep.
                if settling.contains(cancel.entity) {
                    jobs.cancelled(settling_cancelled.id());
                    commands.entity(cancel.entity).despawn();
                } else {
                    commands.trigger(VideoStopRequested {
                        entity: cancel.entity,
                    });
                }
            },
        );
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

#[cfg(test)]
mod tests {
    use bevy::window::PrimaryWindow;
    use serde_json::json;

    use super::*;
    use crate::{
        capture::CapturePlugin,
        command::{Caller, CommandPlugin, CommandScope, CommandWorldExt, Reply},
        time::TimeControlPlugin,
        wait::{Activity, SettleAppExt, WaitPlugin},
    };

    fn always_busy() -> Activity {
        Activity::idle().pending("work", 1)
    }

    #[test]
    fn duration_keeps_its_published_schema_name() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, CommandPlugin, CapturePlugin));
        register(&mut app);
        let registry = app.world().resource::<crate::command::CommandRegistry>();
        let params = &registry.get("video.record").unwrap().params;
        assert_eq!(
            params["properties"]["duration"]["$ref"], "#/$defs/Duration",
            "{params}"
        );
        assert_eq!(
            params["$defs"]["Duration"]["oneOf"]
                .as_array()
                .map(Vec::len),
            Some(3)
        );
    }

    #[test]
    fn cancelling_while_settling_ends_cancelled_with_nothing_recorded() {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            TimeControlPlugin,
            CommandPlugin,
            CapturePlugin,
            WaitPlugin,
        ))
        .register_settle_probe("test", always_busy);
        register(&mut app);
        app.world_mut().spawn((Window::default(), PrimaryWindow));
        let Reply::Job(job) = app
            .world_mut()
            .call_command(
                "video.record",
                json!({
                    "path": "/clip.mp4", "source": "window", "duration": { "frames": 10 },
                    "settle": {},
                }),
                Caller {
                    scope: CommandScope::Development,
                    cwd: None,
                },
            )
            .unwrap()
        else {
            panic!("not a job");
        };
        app.update();
        let status = app
            .world()
            .resource::<Jobs>()
            .status(&job, CommandScope::Development)
            .unwrap();
        assert_eq!(status.progress.unwrap()["phase"], "settling");

        let status = app
            .world_mut()
            .cancel_job(&job, CommandScope::Development)
            .unwrap();
        assert_eq!(status.state, "cancelled");
        app.update();
        let requests = app
            .world_mut()
            .query::<&VideoRequest>()
            .iter(app.world())
            .count();
        assert_eq!(requests, 0);
        let gates = app
            .world_mut()
            .query::<&SettleGate>()
            .iter(app.world())
            .count();
        assert_eq!(gates, 0);
    }
}
