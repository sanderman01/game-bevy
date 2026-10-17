//! Video: record the primary window, or what one camera rendered, to an `.mp4` or `.mkv` file.
//!
//! Spawn a [`VideoRequest`] on a fresh entity. The entity is the request: when the recording
//! finishes, either way, [`VideoFinished`] is triggered on it and it is despawned. Trigger
//! [`VideoStopRequested`] on it to end early and keep what was recorded.
//!
//! ```ignore
//! fn record_game_camera(mut commands: Commands, camera: Single<Entity, With<MainCamera>>) {
//!     commands
//!         .spawn(VideoRequest::new(
//!             CaptureSource::Camera(*camera),
//!             "clips/game.mp4",
//!             VideoLength::Time(Duration::from_secs(10)),
//!         ))
//!         .observe(|finished: On<VideoFinished>| info!("{:?}", finished.result));
//! }
//! ```
//!
//! # How it works
//!
//! When a frame is due, the render world converts the source to NV12 on the GPU after every
//! camera has drawn (see `capture`), copies it into one of a few buffers kept for the recording,
//! and hands the mapped buffer to a writer thread, which writes it straight into an `ffmpeg` child
//! process's stdin. ffmpeg must be on `PATH`; the encoder is the first that works of the hardware
//! ones and `libx264`, probed once per app, and `ENAME_VIDEO_ENCODER` forces one by name. ffmpeg
//! runs at below-normal priority. The game never waits on ffmpeg: a frame with no free buffer is
//! dropped and counted.
//!
//! Timing is real time, so a recording continues while virtual time is paused. In
//! [`VideoLength::Time`] mode a frame is captured only when it is the first to round into a new
//! output tick, and the file is resampled to a constant rate. In [`VideoLength::Frames`] mode
//! every frame is captured until the writer has `n`.
//!
//! The samples are BT.709 at TV range on the sRGB-encoded values, chroma left-sited, and the file
//! is tagged as such (BT.709 primaries and matrix, sRGB transfer). Odd sizes lose their last
//! column or row. A screenshot of the same target in the same frame takes that frame.
//!
//! # Ending early
//!
//! A source that changes size, a camera that goes inactive or disappears, five seconds without a
//! frame, or a stop request each end the recording early. What was recorded is kept and reported
//! as success, with [`SavedVideo::ended`] saying why. Ending before any frame is an error.
//!
//! The `video.record` command runs a request as a job; cancelling that job stops the recording.
//!
//! Despawning a request, or exiting the app, cancels the recording and deletes the partial file
//! without reporting. Once the writer has started moving the finished file into place, cancel
//! loses: the file appears. A hard kill of the game can leave an `.ename-video-*` temp file.

mod capture;
mod command;
mod encoder;
mod length;
mod nv12;

use std::{
    ffi::OsString,
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, atomic::Ordering},
    thread::JoinHandle,
    time::Duration,
};

use bevy::{
    camera::RenderTarget,
    prelude::*,
    render::render_resource::{TextureFormat, TextureUsages},
    window::PrimaryWindow,
};

use crate::capture::CaptureError;
pub use crate::readback::CaptureSource;
use crate::readback::{ReadbackQueue, ReadbackSystems, SourceError, resolve_source};
use capture::{CaptureJob, CaptureShared, PendingCaptures};
use encoder::{
    Container, EncoderSpec, End, WriterJob, WriterShared, encoded_rect, shutdown_writers,
    spawn_writer,
};
pub use length::{VideoLength, parse_timecode};

/// No frame accepted for this long ends a recording as stalled.
const STALL_TIMEOUT: Duration = Duration::from_secs(5);
/// How long an ended recording waits for readbacks already in flight.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
/// Probing every candidate encoder, with margin.
const START_TIMEOUT: Duration = Duration::from_secs(45);
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// One pipe write or wait taking this long means ffmpeg stopped reading.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Everything after the channel closes: queued writes, padding, ffmpeg's exit, fsync.
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long exiting waits for writer threads.
const SHUTDOWN_BOUND: Duration = Duration::from_secs(2);
const MAX_FRAME_RATE: u32 = 240;

/// A recording to make. See the module docs.
#[derive(Component, Clone, Debug)]
#[require(VideoProgress)]
pub struct VideoRequest {
    pub source: CaptureSource,
    /// Must end in `.mp4` or `.mkv`, any case. A relative path resolves against the process's
    /// working directory. Missing parent directories are created and an existing file is
    /// replaced.
    pub path: PathBuf,
    pub length: VideoLength,
    /// Output frames per second, 1 to 240.
    pub frame_rate: NonZeroU32,
}

impl VideoRequest {
    /// A request at 30 frames per second, enough for reviewing motion at half the cost of 60.
    pub fn new(source: CaptureSource, path: impl Into<PathBuf>, length: VideoLength) -> Self {
        Self {
            source,
            path: path.into(),
            length,
            frame_rate: NonZeroU32::new(30).unwrap(),
        }
    }
}

/// Ends a recording early and keeps what was recorded. Before the recording has started it
/// finishes it with [`VideoError::NoFrames`]. Ignored once finalizing.
#[derive(EntityEvent, Clone, Copy, Debug)]
pub struct VideoStopRequested {
    pub entity: Entity,
}

/// Where a request is, for pollers. Written only when a value changes.
#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoProgress {
    pub phase: VideoPhase,
    /// Frames accepted so far.
    pub frames: u32,
    /// Frames requested from the GPU so far, which is also the index the next requested frame
    /// gets. Updated in `Last` as each request is made, so a system that poses the source per
    /// frame reads it in the next frame's `PreUpdate`.
    pub requested: u32,
    pub dropped_frames: u32,
    /// Real time since the first frame was requested.
    pub elapsed: Duration,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VideoPhase {
    /// Choosing an encoder.
    #[default]
    Starting,
    Recording,
    /// Encoding the last frames and closing the file.
    Finalizing,
    /// Finished and reported with [`VideoFinished`].
    Done,
}

/// Why a recording ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoEnd {
    Completed,
    Stopped,
    SourceResized,
    CameraInactive,
    SourceLost,
    Stalled,
}

impl VideoEnd {
    /// Stable snake_case name, such as `source_resized`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Stopped => "stopped",
            Self::SourceResized => "source_resized",
            Self::CameraInactive => "camera_inactive",
            Self::SourceLost => "source_lost",
            Self::Stalled => "stalled",
        }
    }
}

/// A video that was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedVideo {
    pub path: PathBuf,
    /// Encoded size in physical pixels.
    pub size: UVec2,
    /// Frames in the file.
    pub frames: u32,
    pub frame_rate: u32,
    /// `frames / frame_rate`.
    pub duration: Duration,
    /// Frames lost to backpressure, out-of-order delivery, or the readback limit.
    pub dropped_frames: u32,
    /// The ffmpeg encoder used, such as `h264_vaapi`.
    pub encoder: String,
    pub ended: VideoEnd,
}

#[derive(thiserror::Error, Clone, Debug, PartialEq, Eq)]
pub enum VideoError {
    #[error("`{0}` is not a .mp4 or .mkv path")]
    UnsupportedContainer(PathBuf),
    #[error("invalid duration: {0}")]
    InvalidDuration(String),
    #[error("frame rate {0} is not between 1 and 240")]
    InvalidFrameRate(u32),
    #[error("entity {0} is not a camera")]
    NotACamera(Entity),
    #[error("camera {0} is not active this frame, so its target holds a stale image")]
    CameraInactive(Entity),
    #[error("camera {0} renders to a texture view or to nothing, which this API does not capture")]
    UnsupportedTarget(Entity),
    #[error("there is no primary window to capture")]
    NoWindow,
    #[error("the captured texture is {0:?}; only Rgba8UnormSrgb and Bgra8UnormSrgb are supported")]
    UnsupportedFormat(TextureFormat),
    #[error(
        "the camera's target image has usages {0:?}, without TEXTURE_BINDING, so the GPU cannot \
         read it"
    )]
    UnreadableTarget(TextureUsages),
    #[error("the viewport {crop:?} has no area inside the {size} image")]
    EmptyCrop { crop: URect, size: UVec2 },
    #[error("the {size} source is too small to encode once rounded down to even sizes")]
    TooSmall { size: UVec2 },
    #[error("no frame was read back within {0:?}; is the window minimized?")]
    TimedOut(Duration),
    #[error("the recording ended ({0:?}) before any frame was recorded")]
    NoFrames(VideoEnd),
    #[error("ffmpeg is not installed or not on PATH")]
    FfmpegMissing,
    #[error("no working H.264 encoder; tried {}", tried.join(", "))]
    NoEncoder { tried: Vec<String> },
    #[error("ffmpeg failed: {status}{}", if stderr.is_empty() { String::new() } else { format!(": {stderr}") })]
    Ffmpeg { status: String, stderr: String },
    #[error("could not convert a frame: {0}")]
    Convert(String),
    #[error("could not write `{path}`: {message}")]
    Write { path: PathBuf, message: String },
}

impl CaptureError for VideoError {
    fn kind(&self) -> &'static str {
        match self {
            Self::UnsupportedContainer(_) => "unsupported_container",
            Self::InvalidDuration(_) => "invalid_duration",
            Self::InvalidFrameRate(_) => "invalid_frame_rate",
            Self::NotACamera(_) => "not_a_camera",
            Self::CameraInactive(_) => "camera_inactive",
            Self::UnsupportedTarget(_) => "unsupported_target",
            Self::NoWindow => "no_window",
            Self::UnsupportedFormat(_) => "unsupported_format",
            Self::UnreadableTarget(_) => "unreadable_target",
            Self::EmptyCrop { .. } => "empty_crop",
            Self::TooSmall { .. } => "too_small",
            Self::TimedOut(_) => "timed_out",
            Self::NoFrames(_) => "no_frames",
            Self::FfmpegMissing => "ffmpeg_missing",
            Self::NoEncoder { .. } => "no_encoder",
            Self::Ffmpeg { .. } => "ffmpeg",
            Self::Convert(_) => "convert",
            Self::Write { .. } => "write",
        }
    }

    fn is_invalid(&self) -> bool {
        matches!(
            self,
            Self::UnsupportedContainer(_) | Self::InvalidDuration(_) | Self::InvalidFrameRate(_)
        )
    }
}

impl From<SourceError> for VideoError {
    fn from(error: SourceError) -> Self {
        match error {
            SourceError::NotACamera(entity) => Self::NotACamera(entity),
            SourceError::CameraInactive(entity) => Self::CameraInactive(entity),
            SourceError::UnsupportedTarget(entity) => Self::UnsupportedTarget(entity),
            SourceError::NoWindow => Self::NoWindow,
        }
    }
}

/// Triggered once on the request entity when the recording finishes, either way.
#[derive(EntityEvent, Clone, Debug)]
pub struct VideoFinished {
    pub entity: Entity,
    pub result: Result<SavedVideo, VideoError>,
}

/// Whether `path` names a container this records to: `.mp4` or `.mkv`, any case.
pub fn check_video_path(path: &Path) -> Result<(), VideoError> {
    match Container::from_path(path) {
        Some(_) => Ok(()),
        None => Err(VideoError::UnsupportedContainer(path.to_path_buf())),
    }
}

/// `frame_rate` if it is between 1 and 240.
pub fn check_frame_rate(frame_rate: u32) -> Result<NonZeroU32, VideoError> {
    NonZeroU32::new(frame_rate)
        .filter(|rate| rate.get() <= MAX_FRAME_RATE)
        .ok_or(VideoError::InvalidFrameRate(frame_rate))
}

/// Records [`VideoRequest`]s.
pub struct VideoPlugin;

impl Plugin for VideoPlugin {
    fn build(&self, app: &mut App) {
        crate::readback::add_readback_plugin(app);
        app.insert_resource(VideoSettings {
            forced_encoder: std::env::var("ENAME_VIDEO_ENCODER")
                .ok()
                .filter(|name| !name.is_empty()),
            ..default()
        })
        .init_resource::<ActiveWriters>()
        .init_resource::<PendingCaptures>()
        .add_systems(
            Update,
            (
                advance_starting,
                finish_recordings,
                update_progress,
                command::sync_job_progress,
            )
                .chain(),
        )
        .add_systems(
            Last,
            (
                // After this frame's screenshot requests, which it checks for conflicts, and
                // before they are spawned and drained.
                (start_recordings, request_frames)
                    .chain()
                    .after(ReadbackSystems::Request)
                    .before(ReadbackSystems::Spawn),
                shutdown_on_exit,
            ),
        )
        .add_observer(stop_recording);
        capture::build_render(app);
    }

    /// `finish`, so every capture source a plugin added in `build` is in the command's schema.
    fn finish(&self, app: &mut App) {
        command::register(app);
    }
}

/// Timeouts and the ffmpeg program. A resource so tests can shorten them and use fake programs.
#[derive(Resource, Clone)]
struct VideoSettings {
    ffmpeg: OsString,
    forced_encoder: Option<String>,
    encoder_cache: Arc<OnceLock<&'static EncoderSpec>>,
    probe_timeout: Duration,
    start_timeout: Duration,
    stall_timeout: Duration,
    drain_timeout: Duration,
    write_timeout: Duration,
    finalize_timeout: Duration,
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            ffmpeg: "ffmpeg".into(),
            forced_encoder: None,
            encoder_cache: default(),
            probe_timeout: PROBE_TIMEOUT,
            start_timeout: START_TIMEOUT,
            stall_timeout: STALL_TIMEOUT,
            drain_timeout: DRAIN_TIMEOUT,
            write_timeout: WRITE_TIMEOUT,
            finalize_timeout: FINALIZE_TIMEOUT,
        }
    }
}

/// Every writer thread still running. Exiting the app, or dropping this, cancels them all and
/// waits a bounded time.
#[derive(Resource, Default)]
struct ActiveWriters(Vec<(Arc<WriterShared>, JoinHandle<()>)>);

impl Drop for ActiveWriters {
    fn drop(&mut self) {
        shutdown_writers(std::mem::take(&mut self.0), SHUTDOWN_BOUND);
    }
}

/// The main world's side of a writer. Dropping it cancels the writer.
struct WriterHandle(Arc<WriterShared>);

impl Drop for WriterHandle {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Where a request is. A request without one has not started. Times are `Time<Real>` elapsed.
#[derive(Component)]
enum RecordingStage {
    /// The writer is choosing an encoder. Nothing is requested and no stall clock runs.
    Starting {
        writer: WriterHandle,
        capture: Arc<CaptureShared>,
        since: Duration,
        stop: bool,
    },
    Recording(Box<Recorder>),
    /// The channel is closed; the writer encodes the tail and closes the file by `deadline`.
    Finalizing {
        writer: WriterHandle,
        capture: Arc<CaptureShared>,
        deadline: Duration,
        skipped: u64,
    },
    Done,
}

struct Recorder {
    writer: WriterHandle,
    capture: Arc<CaptureShared>,
    length: VideoLength,
    frame_rate: u32,
    /// The first request's encoded size, to spot a resize.
    first_size: Option<UVec2>,
    /// Real time of the first request. Frame stamps are relative to it.
    t0: Option<Duration>,
    /// Time mode: the output tick of the last frame requested.
    last_tick: Option<u64>,
    next_seq: u64,
    /// Frames not requested because a screenshot took the target's readback that frame.
    skipped: u64,
    /// Why and when it ended. Requests stop; frames in flight still count.
    end: Option<(VideoEnd, Duration)>,
    /// A problem that fails the recording.
    failure: Option<VideoError>,
}

impl Recorder {
    fn end(&mut self, reason: VideoEnd, now: Duration) {
        if self.end.is_none() {
            self.end = Some((reason, now));
        }
    }

    fn dropped(&self) -> u64 {
        self.capture.failed() + self.writer.0.rejected.load(Ordering::Acquire) + self.skipped
    }
}

fn start_recordings(
    mut commands: Commands,
    requests: Query<(Entity, &VideoRequest), Without<RecordingStage>>,
    cameras: Query<(&Camera, &RenderTarget)>,
    primary_window: Query<Entity, With<PrimaryWindow>>,
    settings: Res<VideoSettings>,
    mut writers: ResMut<ActiveWriters>,
    time: Res<Time<Real>>,
) {
    let now = time.elapsed();
    let primary_window = primary_window.single().ok();
    for (entity, request) in &requests {
        let checked = (|| {
            let container = Container::from_path(&request.path)
                .ok_or_else(|| VideoError::UnsupportedContainer(request.path.clone()))?;
            request.length.validate()?;
            check_frame_rate(request.frame_rate.get())?;
            resolve_source(request.source, &cameras, primary_window)?;
            Ok(container)
        })();
        let container = match checked {
            Ok(container) => container,
            Err(err) => {
                commands.entity(entity).try_insert(RecordingStage::Done);
                report(&mut commands, entity, Err(err));
                continue;
            }
        };

        let (capture, receiver) = CaptureShared::new();
        let (shared, thread) = spawn_writer(WriterJob {
            path: request.path.clone(),
            container,
            length: request.length,
            frame_rate: request.frame_rate.get(),
            frames: receiver,
            ffmpeg: settings.ffmpeg.clone(),
            forced_encoder: settings.forced_encoder.clone(),
            probe_timeout: settings.probe_timeout,
            stall_timeout: settings.stall_timeout,
            encoder_cache: settings.encoder_cache.clone(),
        });
        writers.0.push((shared.clone(), thread));
        commands
            .entity(entity)
            .try_insert(RecordingStage::Starting {
                writer: WriterHandle(shared),
                capture,
                since: now,
                stop: false,
            });
    }
}

/// Moves requests whose writer has an encoder to recording. The stall clock starts here, so a
/// slow probe never reads as a stall.
fn advance_starting(
    mut commands: Commands,
    mut requests: Query<(Entity, &VideoRequest, &mut RecordingStage)>,
    settings: Res<VideoSettings>,
    time: Res<Time<Real>>,
) {
    let now = time.elapsed();
    for (entity, request, mut stage) in &mut requests {
        let RecordingStage::Starting {
            writer,
            since,
            stop,
            ..
        } = &*stage
        else {
            continue;
        };
        let failure = if *stop {
            Some(VideoError::NoFrames(VideoEnd::Stopped))
        } else if let Some(Err(err)) = writer.0.ready.get() {
            Some(err.clone())
        } else if writer.0.ready.get().is_none() && now >= *since + settings.start_timeout {
            Some(VideoError::NoEncoder {
                tried: vec![format!(
                    "probing took longer than {:?}",
                    settings.start_timeout
                )],
            })
        } else {
            None
        };
        if let Some(err) = failure {
            // Dropping the stage's writer handle cancels the writer.
            *stage = RecordingStage::Done;
            report(&mut commands, entity, Err(err));
            continue;
        }
        if writer.0.ready.get().is_none() {
            continue;
        }
        let RecordingStage::Starting {
            writer, capture, ..
        } = std::mem::replace(&mut *stage, RecordingStage::Done)
        else {
            unreachable!();
        };
        writer.0.reset_accepted_clock();
        *stage = RecordingStage::Recording(Box::new(Recorder {
            writer,
            capture,
            length: request.length,
            frame_rate: request.frame_rate.get(),
            first_size: None,
            t0: None,
            last_tick: None,
            next_seq: 0,
            skipped: 0,
            end: None,
            failure: None,
        }));
    }
}

/// Asks the render world for this frame's captures.
///
/// Frames mode asks every frame until the writer has taken `n`. Time mode asks for the first frame
/// whose stamp rounds into a new output tick (`round((now - t0) * fps)`, as `ticks_until` rounds),
/// so readback and encoding run at the lower of the game's and the output's frame rate. At a high
/// render rate the chosen frame can be up to half a tick early. A frame that is never delivered
/// leaves its tick to the previous frame.
fn request_frames(
    mut requests: Query<(&VideoRequest, &mut RecordingStage, &mut VideoProgress)>,
    cameras: Query<(&Camera, &RenderTarget)>,
    windows: Query<(Entity, &Window), With<PrimaryWindow>>,
    screenshots: Res<ReadbackQueue>,
    mut pending: ResMut<PendingCaptures>,
    settings: Res<VideoSettings>,
    time: Res<Time<Real>>,
) {
    pending.jobs.clear();
    pending.warm = false;
    let now = time.elapsed();
    let primary_window = windows.single().ok();
    for (request, mut stage, mut progress) in &mut requests {
        let recorder = match &mut *stage {
            RecordingStage::Recording(recorder) => recorder,
            RecordingStage::Starting { .. } => {
                pending.warm = true;
                continue;
            }
            _ => continue,
        };
        pending.warm = true;
        if recorder.end.is_some() || recorder.failure.is_some() {
            continue;
        }
        if let Some(error) = recorder.capture.error.get() {
            recorder.failure = Some(error.clone());
            continue;
        }
        // First, so a source that never resolves or never lays out still stalls out.
        if recorder.writer.0.since_accepted() > settings.stall_timeout {
            recorder.end(VideoEnd::Stalled, now);
            continue;
        }

        let source = match resolve_source(
            request.source,
            &cameras,
            primary_window.map(|(entity, _)| entity),
        ) {
            Ok(source) => source,
            Err(SourceError::CameraInactive(_)) => {
                recorder.end(VideoEnd::CameraInactive, now);
                continue;
            }
            Err(_) => {
                recorder.end(VideoEnd::SourceLost, now);
                continue;
            }
        };
        let target_size = match request.source {
            CaptureSource::PrimaryWindow => {
                primary_window.map(|(_, window)| window.physical_size())
            }
            CaptureSource::Camera(camera) => cameras
                .get(camera)
                .ok()
                .and_then(|(camera, _)| camera.physical_target_size()),
        };
        // Not laid out yet: nothing to capture this frame.
        let Some(target_size) = target_size else {
            continue;
        };
        let rect = match encoded_rect(target_size, source.crop) {
            Ok(rect) => rect,
            Err(_) if recorder.first_size.is_some() => {
                recorder.end(VideoEnd::SourceResized, now);
                continue;
            }
            Err(error) => {
                recorder.failure = Some(error);
                continue;
            }
        };
        match recorder.first_size {
            None => recorder.first_size = Some(rect.size()),
            Some(first) if first != rect.size() => {
                recorder.end(VideoEnd::SourceResized, now);
                continue;
            }
            Some(_) => {}
        }
        let t0 = *recorder.t0.get_or_insert(now);
        let stamp = now - t0;
        let mut completes = false;
        match recorder.length {
            VideoLength::Frames(limit) => {
                let limit = u64::from(limit.get());
                if recorder.writer.0.accepted.load(Ordering::Acquire) >= limit {
                    recorder.end(VideoEnd::Completed, now);
                    continue;
                }
                // Enough is already on its way; asking for more would only be surplus.
                let live = recorder
                    .capture
                    .requested
                    .load(Ordering::Acquire)
                    .saturating_sub(recorder.capture.failed())
                    .saturating_sub(recorder.writer.0.rejected.load(Ordering::Acquire));
                if live >= limit {
                    continue;
                }
            }
            VideoLength::Time(length) => {
                let tick = encoder::ticks_until(stamp, recorder.frame_rate, 0, u64::MAX).end;
                if recorder.last_tick.is_some_and(|last| tick <= last) {
                    continue;
                }
                completes = stamp >= length;
            }
        }
        // A screenshot redirects the same target this frame; Bevy keeps one redirect.
        if screenshots.is_queued(&source.key) {
            recorder.skipped += 1;
            continue;
        }
        if let VideoLength::Time(_) = recorder.length {
            recorder.last_tick =
                Some(encoder::ticks_until(stamp, recorder.frame_rate, 0, u64::MAX).end);
        }
        let requested = recorder.capture.requested.fetch_add(1, Ordering::AcqRel) + 1;
        progress.requested = requested as u32;
        pending.jobs.push(CaptureJob {
            shared: recorder.capture.clone(),
            key: source.key,
            rect,
            seq: recorder.next_seq,
            stamp,
        });
        recorder.next_seq += 1;
        if completes {
            recorder.end(VideoEnd::Completed, now);
        }
    }
}

/// A request that has not started yet finishes right here, so a stop sent in the same frame as
/// the request is not lost.
fn stop_recording(
    stop: On<VideoStopRequested>,
    mut requests: Query<Option<&mut RecordingStage>, With<VideoRequest>>,
    time: Res<Time<Real>>,
    mut commands: Commands,
) {
    let Ok(stage) = requests.get_mut(stop.entity) else {
        return;
    };
    let Some(mut stage) = stage else {
        commands
            .entity(stop.entity)
            .try_insert(RecordingStage::Done);
        report(
            &mut commands,
            stop.entity,
            Err(VideoError::NoFrames(VideoEnd::Stopped)),
        );
        return;
    };
    match &mut *stage {
        RecordingStage::Starting { stop, .. } => *stop = true,
        RecordingStage::Recording(recorder) => recorder.end(VideoEnd::Stopped, time.elapsed()),
        RecordingStage::Finalizing { .. } | RecordingStage::Done => {}
    }
}

fn finish_recordings(
    mut commands: Commands,
    mut requests: Query<(Entity, &mut RecordingStage)>,
    mut writers: ResMut<ActiveWriters>,
    settings: Res<VideoSettings>,
    time: Res<Time<Real>>,
) {
    let now = time.elapsed();
    for (entity, mut stage) in &mut requests {
        let mut closing = None;
        let outcome = match &mut *stage {
            RecordingStage::Recording(recorder) => {
                if let Some(result) = recorder.writer.0.result.get() {
                    // The writer stopped on its own while frames were still coming: an error.
                    Some(result.clone())
                } else if let Some(failure) = &recorder.failure {
                    Some(Err(failure.clone()))
                } else if let Some(error) = recorder.capture.error.get() {
                    // Found by a request after the recording had ended, such as the last timed
                    // one: it still fails the recording.
                    Some(Err(error.clone()))
                } else if recorder.writer.0.stuck_for() > settings.write_timeout {
                    Some(Err(stalled_ffmpeg("ffmpeg stopped reading frames")))
                } else {
                    closing = recorder.end.and_then(|(reason, ended_at)| {
                        (recorder.capture.in_flight() == 0
                            || now >= ended_at + settings.drain_timeout)
                            .then_some(reason)
                    });
                    None
                }
            }
            RecordingStage::Finalizing {
                writer,
                capture,
                deadline,
                skipped,
            } => match writer.0.result.get() {
                // A late render-side error wins over what the writer saved.
                Some(_) if capture.error.get().is_some() => {
                    Some(Err(capture.error.get().unwrap().clone()))
                }
                Some(result) => {
                    log_drops(capture, &writer.0, *skipped);
                    Some(result.clone().map(|mut saved| {
                        let dropped =
                            capture.failed() + writer.0.rejected.load(Ordering::Acquire) + *skipped;
                        saved.dropped_frames = dropped as u32;
                        saved
                    }))
                }
                None if now >= *deadline => Some(Err(stalled_ffmpeg(&format!(
                    "finalizing took longer than {:?}",
                    settings.finalize_timeout
                )))),
                None => None,
            },
            _ => None,
        };
        if let Some(reason) = closing {
            close(&mut stage, reason, now + settings.finalize_timeout);
        }
        if let Some(result) = outcome {
            // Dropping the writer handle cancels a writer that is still running.
            if let RecordingStage::Recording(recorder) = &*stage {
                recorder.capture.close();
            }
            *stage = RecordingStage::Done;
            report(&mut commands, entity, result);
        }
    }
    writers.0.retain(|(_, thread)| !thread.is_finished());
}

/// Stops capturing and gives the writer its end. The writer's channel closes once every frame
/// still in flight has been delivered or dropped.
fn close(stage: &mut RecordingStage, reason: VideoEnd, deadline: Duration) {
    let RecordingStage::Recording(recorder) = std::mem::replace(stage, RecordingStage::Done) else {
        return;
    };
    let Recorder {
        writer,
        capture,
        skipped,
        ..
    } = *recorder;
    writer.0.end(End { reason });
    capture.close();
    *stage = RecordingStage::Finalizing {
        writer,
        capture,
        deadline,
        skipped,
    };
}

/// Logs why frames were dropped, by reason, so a recording with drops can be diagnosed from the
/// log (`log_get_entries` with `target_contains: "video"`).
fn log_drops(capture: &CaptureShared, writer: &WriterShared, skipped: u64) {
    let mut reasons: Vec<String> = capture
        .drops()
        .filter(|(_, count)| *count > 0)
        .map(|(name, count)| format!("{name}={count}"))
        .collect();
    let rejected = writer.rejected.load(Ordering::Acquire);
    if rejected > 0 {
        reasons.push(format!("out_of_order={rejected}"));
    }
    if skipped > 0 {
        reasons.push(format!("screenshot={skipped}"));
    }
    let requested = capture.requested.load(Ordering::Acquire);
    let delivered = capture.delivered.load(Ordering::Acquire);
    if reasons.is_empty() {
        debug!("video: {delivered} of {requested} requested frames delivered, none dropped");
    } else {
        info!(
            "video: {delivered} of {requested} requested frames delivered; dropped {}",
            reasons.join(" ")
        );
    }
}

fn stalled_ffmpeg(status: &str) -> VideoError {
    VideoError::Ffmpeg {
        status: status.to_owned(),
        stderr: String::new(),
    }
}

fn update_progress(
    mut requests: Query<(&RecordingStage, &mut VideoProgress)>,
    time: Res<Time<Real>>,
) {
    let now = time.elapsed();
    for (stage, mut progress) in &mut requests {
        let next = match stage {
            RecordingStage::Starting { .. } => VideoProgress::default(),
            RecordingStage::Recording(recorder) => VideoProgress {
                phase: VideoPhase::Recording,
                frames: recorder.writer.0.accepted.load(Ordering::Acquire) as u32,
                requested: recorder.capture.requested.load(Ordering::Acquire) as u32,
                dropped_frames: recorder.dropped() as u32,
                elapsed: recorder.t0.map(|t0| now - t0).unwrap_or_default(),
            },
            RecordingStage::Finalizing { .. } => VideoProgress {
                phase: VideoPhase::Finalizing,
                ..progress.clone()
            },
            RecordingStage::Done => VideoProgress {
                phase: VideoPhase::Done,
                ..progress.clone()
            },
        };
        progress.set_if_neq(next);
    }
}

fn shutdown_on_exit(mut exits: MessageReader<AppExit>, mut writers: ResMut<ActiveWriters>) {
    if exits.read().next().is_some() {
        shutdown_writers(std::mem::take(&mut writers.0), SHUTDOWN_BOUND);
    }
}

/// Publishes a finished request and despawns it; the caller has already moved it to `Done`.
fn report(commands: &mut Commands, entity: Entity, result: Result<SavedVideo, VideoError>) {
    commands.trigger(VideoFinished { entity, result });
    commands.entity(entity).try_despawn();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::screenshot::{ScreenshotPlugin, ScreenshotRequest};
    use bevy::window::WindowResolution;
    use encoder::tests::{fake_exited, fake_ffmpeg, lock_processes};
    use nv12::frame_len;
    use std::time::Instant;

    #[derive(Resource, Default)]
    struct Finished(Vec<(Entity, Result<SavedVideo, VideoError>)>);

    const CAT: &str = "exec cat > \"$out\"";
    /// The test window: 320x240, so one NV12 frame (115 KB) fills a pipe.
    const SIZE: UVec2 = UVec2::new(320, 240);

    fn app(
        dir: &Path,
        probe: &str,
        encode: &str,
        settings: impl FnOnce(&mut VideoSettings),
    ) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, VideoPlugin))
            .init_resource::<Finished>()
            .add_observer(|finished: On<VideoFinished>, mut seen: ResMut<Finished>| {
                seen.0.push((finished.entity, finished.result.clone()));
            });
        let mut video = VideoSettings {
            ffmpeg: fake_ffmpeg(dir, probe, encode),
            forced_encoder: Some("libx264".into()),
            ..default()
        };
        settings(&mut video);
        app.insert_resource(video);
        app.world_mut().spawn((
            Window {
                resolution: WindowResolution::new(SIZE.x, SIZE.y),
                ..default()
            },
            PrimaryWindow,
        ));
        app
    }

    fn record(app: &mut App, path: PathBuf, length: VideoLength) -> Entity {
        app.world_mut()
            .spawn(VideoRequest::new(
                CaptureSource::PrimaryWindow,
                path,
                length,
            ))
            .id()
    }

    fn frames(n: u32) -> VideoLength {
        VideoLength::Frames(NonZeroU32::new(n).unwrap())
    }

    fn phase(app: &App, entity: Entity) -> VideoPhase {
        app.world().get::<VideoProgress>(entity).unwrap().phase
    }

    fn update_until(app: &mut App, what: &str, mut done: impl FnMut(&mut App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.update();
            if done(app) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn recording(app: &mut App, entity: Entity) {
        update_until(app, "recording", |app| {
            phase(app, entity) == VideoPhase::Recording
        });
    }

    fn finished(app: &mut App) -> Result<SavedVideo, VideoError> {
        update_until(app, "the recording to finish", |app| {
            !app.world().resource::<Finished>().0.is_empty()
        });
        let seen = &app.world().resource::<Finished>().0;
        assert_eq!(seen.len(), 1, "{seen:?}");
        seen[0].1.clone()
    }

    /// This frame's capture requests, as the render world would see them.
    fn jobs(app: &App) -> Vec<CaptureJob> {
        app.world().resource::<PendingCaptures>().jobs.clone()
    }

    /// Delivers `jobs` as completed maps would, in one go.
    fn deliver(jobs: &[CaptureJob]) {
        for job in jobs {
            let len = frame_len(job.rect.size()) as usize;
            job.shared.deliver_bytes(job, vec![128; len]);
        }
    }

    /// One update, then delivers what it requested.
    fn update_and_deliver(app: &mut App) -> usize {
        app.update();
        let jobs = jobs(app);
        deliver(&jobs);
        jobs.len()
    }

    fn file_frames(path: &Path) -> u64 {
        std::fs::metadata(path).unwrap().len() / frame_len(SIZE)
    }

    #[test]
    fn a_bad_path_finishes_once() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path(), "exit 0", CAT, |_| {});
        record(&mut app, dir.path().join("a.webm"), frames(1));
        assert!(matches!(
            finished(&mut app),
            Err(VideoError::UnsupportedContainer(_))
        ));
    }

    #[test]
    fn frames_mode_writes_exactly_n_from_a_late_batch() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path(), "exit 0", CAT, |_| {});
        let path = dir.path().join("a.mp4");
        let entity = record(&mut app, path.clone(), frames(2));
        recording(&mut app, entity);
        // Nothing delivered yet: only n frames are asked for, then they arrive together.
        let mut batch = jobs(&app);
        for _ in 0..3 {
            app.update();
            batch.extend(jobs(&app));
        }
        assert_eq!(batch.len(), 2);
        assert!(batch.iter().all(|job| job.rect.size() == SIZE));
        deliver(&batch);
        let saved = finished(&mut app).unwrap();
        assert_eq!((saved.frames, saved.ended), (2, VideoEnd::Completed));
        assert_eq!(saved.dropped_frames, 0);
        assert_eq!(file_frames(&path), 2);
    }

    #[test]
    fn a_stop_before_start_is_not_lost() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path(), "exit 0", CAT, |_| {});
        let entity = record(&mut app, dir.path().join("a.mp4"), frames(5));
        app.world_mut().trigger(VideoStopRequested { entity });
        assert_eq!(
            finished(&mut app),
            Err(VideoError::NoFrames(VideoEnd::Stopped))
        );
        assert!(jobs(&app).is_empty());
    }

    #[test]
    fn an_early_stop_keeps_only_what_was_recorded() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path(), "exit 0", CAT, |_| {});
        let path = dir.path().join("a.mp4");
        let entity = record(
            &mut app,
            path.clone(),
            VideoLength::Time(Duration::from_secs(3600)),
        );
        // Before `recording`, which already requests the first frame and so sets `t0`.
        let started = Instant::now();
        recording(&mut app, entity);
        deliver(&jobs(&app));
        for _ in 0..2 {
            std::thread::sleep(Duration::from_millis(40));
            update_and_deliver(&mut app);
        }
        let recorded = started.elapsed();
        app.world_mut().trigger(VideoStopRequested { entity });
        let saved = finished(&mut app).unwrap();
        assert_eq!(saved.ended, VideoEnd::Stopped);
        // Ticks up to one period past the last frame: bounded by the wall time recorded, at
        // 30 fps. Nowhere near an hour.
        let bound = (recorded.as_secs_f64() * 30.0).ceil() as u32 + 2;
        assert!(
            (1..=bound).contains(&saved.frames),
            "{} frames in {recorded:?}",
            saved.frames
        );
        assert_eq!(file_frames(&path), u64::from(saved.frames));
    }

    #[test]
    fn a_size_change_ends_the_recording_and_keeps_it() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path(), "exit 0", CAT, |_| {});
        let entity = record(&mut app, dir.path().join("a.mp4"), frames(10));
        recording(&mut app, entity);
        deliver(&jobs(&app));
        let mut window = app
            .world_mut()
            .query_filtered::<&mut Window, With<PrimaryWindow>>()
            .single_mut(app.world_mut())
            .unwrap();
        window.resolution.set_physical_resolution(400, 300);
        let saved = finished(&mut app).unwrap();
        assert_eq!(saved.ended, VideoEnd::SourceResized);
        assert!(saved.frames >= 1 && saved.size == SIZE);
    }

    #[test]
    fn a_slow_probe_is_not_a_stall() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path(), "sleep 0.5; exit 0", CAT, |settings| {
            settings.stall_timeout = Duration::from_millis(100);
        });
        let entity = record(&mut app, dir.path().join("a.mp4"), frames(1));
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(300) {
            app.update();
            assert_eq!(phase(&app, entity), VideoPhase::Starting);
            assert!(jobs(&app).is_empty());
            std::thread::sleep(Duration::from_millis(10));
        }
        recording(&mut app, entity);
        deliver(&jobs(&app));
        assert_eq!(finished(&mut app).unwrap().frames, 1);
    }

    #[test]
    fn time_mode_requests_once_per_output_tick() {
        use bevy::time::TimeUpdateStrategy;
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path(), "exit 0", CAT, |_| {});
        let entity = app
            .world_mut()
            .spawn(VideoRequest {
                frame_rate: NonZeroU32::new(10).unwrap(),
                ..VideoRequest::new(
                    CaptureSource::PrimaryWindow,
                    dir.path().join("a.mp4"),
                    VideoLength::Time(Duration::from_secs(10)),
                )
            })
            .id();
        recording(&mut app, entity);
        // From here on, real time advances 5 ms per update regardless of the machine's load.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(5)));
        let mut requested = jobs(&app).len();
        deliver(&jobs(&app));
        for _ in 0..66 {
            requested += update_and_deliver(&mut app);
        }
        // About 330 ms at 10 fps: the frames that round into ticks 0, 1, 2 and 3, one each.
        assert!((4..=5).contains(&requested), "{requested} requests");
    }

    #[test]
    fn failed_captures_are_counted_and_do_not_block_the_drain() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path(), "exit 0", CAT, |_| {});
        let entity = record(&mut app, dir.path().join("a.mp4"), frames(3));
        recording(&mut app, entity);
        deliver(&jobs(&app));
        app.update();
        for job in jobs(&app) {
            job.shared.fail(capture::DropReason::NoSlot);
        }
        update_and_deliver(&mut app);
        app.world_mut().trigger(VideoStopRequested { entity });
        let saved = finished(&mut app).unwrap();
        assert_eq!((saved.frames, saved.dropped_frames), (2, 1));
    }

    #[test]
    fn a_render_side_error_fails_the_recording() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path(), "exit 0", CAT, |_| {});
        let entity = record(&mut app, dir.path().join("a.mp4"), frames(3));
        recording(&mut app, entity);
        let job = jobs(&app).remove(0);
        let error = VideoError::UnsupportedFormat(TextureFormat::Rgba16Float);
        job.shared.error.set(error.clone()).unwrap();
        job.shared.fail(capture::DropReason::NoSlot);
        assert_eq!(finished(&mut app), Err(error));
    }

    /// An error found by the last request, after the recording has ended, still fails it
    /// rather than saving what came before as a completed recording.
    #[test]
    fn a_render_error_after_the_end_fails_the_recording() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path(), "exit 0", CAT, |_| {});
        let entity = record(
            &mut app,
            dir.path().join("a.mp4"),
            VideoLength::Time(Duration::from_secs(3600)),
        );
        recording(&mut app, entity);
        let first = jobs(&app);
        deliver(&first);
        // The recording has ended; the render side then reports a problem with its source.
        app.world_mut().trigger(VideoStopRequested { entity });
        let error = VideoError::UnreadableTarget(TextureUsages::RENDER_ATTACHMENT);
        first[0].shared.error.set(error.clone()).unwrap();
        assert_eq!(finished(&mut app), Err(error));
    }

    #[test]
    fn a_screenshot_of_the_same_target_takes_the_frame() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path(), "exit 0", CAT, |_| {});
        app.add_plugins(ScreenshotPlugin);
        let entity = record(&mut app, dir.path().join("a.mp4"), frames(5));
        recording(&mut app, entity);
        app.world_mut().spawn(ScreenshotRequest {
            source: CaptureSource::PrimaryWindow,
            path: dir.path().join("a.png"),
        });
        app.update();
        assert!(jobs(&app).is_empty());
        app.update();
        assert_eq!(
            app.world()
                .get::<VideoProgress>(entity)
                .unwrap()
                .dropped_frames,
            1
        );
        app.update();
        assert_eq!(jobs(&app).len(), 1);
    }

    #[test]
    fn exiting_stops_an_idle_writer() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path(), "exit 0", CAT, |_| {});
        let entity = record(&mut app, dir.path().join("a.mp4"), frames(5));
        recording(&mut app, entity);
        let started = Instant::now();
        app.world_mut().write_message(AppExit::Success);
        app.update();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(app.world().resource::<ActiveWriters>().0.is_empty());
        assert!(app.world().resource::<Finished>().0.is_empty());
    }

    #[test]
    fn dropping_the_world_stops_a_blocked_writer() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path(), "exit 0", "exec sleep 30", |_| {});
        let entity = record(&mut app, dir.path().join("a.mp4"), frames(100));
        recording(&mut app, entity);
        // 115 KB frames: the pipe fills and the writer blocks in a write.
        deliver(&jobs(&app));
        for _ in 0..3 {
            update_and_deliver(&mut app);
        }
        let started = Instant::now();
        drop(app);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(fake_exited(dir.path()));
        assert!(!dir.path().read_dir().unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".ename-video-")
        }));
    }

    #[test]
    fn ffmpeg_that_stops_reading_fails_the_recording() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(
            dir.path(),
            "exit 0",
            "head -c 65536 > /dev/null; exec sleep 30",
            |settings| settings.write_timeout = Duration::from_millis(300),
        );
        let entity = record(&mut app, dir.path().join("a.mp4"), frames(100));
        recording(&mut app, entity);
        deliver(&jobs(&app));
        update_until(&mut app, "the watchdog", |app| {
            deliver(&jobs(app));
            !app.world().resource::<Finished>().0.is_empty()
        });
        let result = &app.world().resource::<Finished>().0[0].1;
        assert!(
            matches!(result, Err(VideoError::Ffmpeg { status, .. }) if status.contains("stopped reading")),
            "{result:?}"
        );
        drop(app);
        assert!(fake_exited(dir.path()));
    }

    #[test]
    fn slow_finalizing_hits_the_deadline() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(
            dir.path(),
            "exit 0",
            "while [ \"$(head -c 65536 | wc -c)\" -gt 0 ]; do sleep 0.2; done",
            |settings| settings.finalize_timeout = Duration::from_millis(300),
        );
        let entity = record(&mut app, dir.path().join("a.mp4"), frames(6));
        recording(&mut app, entity);
        deliver(&jobs(&app));
        update_until(&mut app, "the finalize deadline", |app| {
            deliver(&jobs(app));
            !app.world().resource::<Finished>().0.is_empty()
        });
        let result = &app.world().resource::<Finished>().0[0].1;
        assert!(
            matches!(result, Err(VideoError::Ffmpeg { status, .. }) if status.contains("finalizing")),
            "{result:?}"
        );
        drop(app);
        assert!(fake_exited(dir.path()));
    }

    /// [`app`] with the command registry, the capture sources and `video.record`.
    fn command_app(dir: &Path) -> App {
        let mut app = app(dir, "exit 0", CAT, |_| {});
        app.add_plugins((crate::command::CommandPlugin, crate::capture::CapturePlugin));
        command::register(&mut app);
        app
    }

    fn start_command(app: &mut App, params: serde_json::Value) -> (crate::command::JobId, Entity) {
        use crate::command::{Caller, CommandScope, CommandWorldExt, Reply};
        let reply = app
            .world_mut()
            .call_command(
                "video.record",
                params,
                Caller {
                    scope: CommandScope::Development,
                    cwd: None,
                },
            )
            .unwrap();
        let Reply::Job(job) = reply else {
            panic!("not a job: {reply:?}");
        };
        let entity = app
            .world_mut()
            .query_filtered::<Entity, With<VideoRequest>>()
            .single(app.world())
            .unwrap();
        (job, entity)
    }

    fn job_status(app: &App, job: &crate::command::JobId) -> crate::command::JobStatus {
        app.world()
            .resource::<crate::command::Jobs>()
            .status(job, crate::command::CommandScope::Development)
            .unwrap()
    }

    fn cancel(app: &mut App, job: &crate::command::JobId) {
        use crate::command::{CommandScope, CommandWorldExt};
        app.world_mut()
            .cancel_job(job, CommandScope::Development)
            .unwrap();
    }

    #[test]
    fn the_command_records_and_reports_progress() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = command_app(dir.path());
        let path = dir.path().join("a.mp4");
        let (job, entity) = start_command(
            &mut app,
            serde_json::json!({ "path": path, "source": "window", "duration": { "frames": 2 } }),
        );
        recording(&mut app, entity);
        let mut batch = jobs(&app);
        for _ in 0..3 {
            app.update();
            batch.extend(jobs(&app));
        }
        assert_eq!(
            job_status(&app, &job).progress.unwrap()["phase"],
            "recording"
        );
        deliver(&batch);
        finished(&mut app).unwrap();
        let status = job_status(&app, &job);
        assert_eq!(status.state, "succeeded", "{status:?}");
        let result = status.result.unwrap();
        assert_eq!(result["ended"], "completed");
        assert_eq!(result["frames"], 2);
        assert!(result.get("note").is_none());
    }

    #[test]
    fn cancelling_the_job_stops_and_keeps_the_recording() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = command_app(dir.path());
        let path = dir.path().join("a.mp4");
        let (job, entity) = start_command(
            &mut app,
            serde_json::json!({ "path": path, "source": "window", "duration": { "seconds": 3600 } }),
        );
        recording(&mut app, entity);
        deliver(&jobs(&app));
        std::thread::sleep(Duration::from_millis(40));
        update_and_deliver(&mut app);
        cancel(&mut app, &job);
        finished(&mut app).unwrap();
        let status = job_status(&app, &job);
        assert_eq!(status.state, "succeeded", "{status:?}");
        let result = status.result.unwrap();
        assert_eq!(result["ended"], "stopped");
        assert!(result["note"].as_str().unwrap().contains("ended early"));
        assert!(path.exists());
    }

    #[test]
    fn cancelling_before_the_first_frame_ends_cancelled() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = command_app(dir.path());
        let path = dir.path().join("a.mp4");
        let (job, _) = start_command(
            &mut app,
            serde_json::json!({ "path": path, "source": "window", "duration": { "frames": 5 } }),
        );
        cancel(&mut app, &job);
        assert_eq!(
            finished(&mut app),
            Err(VideoError::NoFrames(VideoEnd::Stopped))
        );
        assert_eq!(job_status(&app, &job).state, "cancelled");
        assert!(!path.exists());
    }

    #[test]
    fn the_command_refuses_bad_params_before_spawning() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = command_app(dir.path());
        let base = |path: &str, duration: serde_json::Value| serde_json::json!({ "path": path, "source": "window", "duration": duration });
        for (params, kind) in [
            (
                base("/tmp/a.webm", serde_json::json!({ "frames": 1 })),
                "unsupported_container",
            ),
            (
                base("/tmp/a.mp4", serde_json::json!({ "frames": 0 })),
                "invalid_duration",
            ),
            (
                base("/tmp/a.mp4", serde_json::json!({ "seconds": -1 })),
                "invalid_duration",
            ),
            (
                base("/tmp/a.mp4", serde_json::json!({ "timecode": "x" })),
                "invalid_duration",
            ),
            (
                base(
                    "/tmp/a.mp4",
                    serde_json::json!({ "frames": 1, "seconds": 1 }),
                ),
                "invalid_params",
            ),
            (base("/tmp/a.mp4", serde_json::json!(10)), "invalid_params"),
            (
                serde_json::json!({ "path": "/tmp/a.mp4", "source": "window", "duration": { "frames": 1 }, "frame_rate": 241 }),
                "invalid_frame_rate",
            ),
            (
                serde_json::json!({ "path": "/tmp/a.mp4", "source": "moon", "duration": { "frames": 1 } }),
                "unknown_source",
            ),
        ] {
            use crate::command::{Caller, CommandScope, CommandWorldExt};
            let err = app
                .world_mut()
                .call_command(
                    "video.record",
                    params.clone(),
                    Caller {
                        scope: CommandScope::Development,
                        cwd: None,
                    },
                )
                .unwrap_err();
            assert_eq!(err.kind, kind, "{params}");
        }
        assert_eq!(
            app.world_mut()
                .query::<&VideoRequest>()
                .iter(app.world())
                .count(),
            0
        );
    }
}
