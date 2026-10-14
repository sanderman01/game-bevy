//! Video: record the primary window, or what one camera rendered, to an `.mp4` or `.mkv` file.
//!
//! Spawn a [`VideoRequest`] on a fresh entity. The entity is the request: when the recording
//! finishes, either way, [`VideoFinished`] is triggered on it and it is despawned, unless it also
//! has [`RetainVideoOutcome`]. Trigger [`VideoStopRequested`] on it to end early and keep what was
//! recorded.
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
//! Each frame the request asks for a GPU readback of its source through the readback queue it
//! shares with screenshots. Delivered frames go over a bounded channel to a writer thread, which
//! crops them and streams raw pixels to an `ffmpeg` child process. ffmpeg must be on `PATH`; the
//! encoder is the first that works of the hardware ones and `libx264`, probed once per app.
//! `ENAME_VIDEO_ENCODER` forces one by name. The game never waits on ffmpeg: a frame that would
//! not fit in the queue is dropped and counted.
//!
//! Timing is real time, so a recording continues while virtual time is paused. In
//! [`VideoLength::Time`] mode frames are resampled to a constant rate. In
//! [`VideoLength::Frames`] mode each accepted frame is one video frame.
//!
//! The pixels are sRGB, and the file is tagged as such (BT.709 primaries and matrix, sRGB
//! transfer). Odd sizes lose their last column or row.
//!
//! # Ending early
//!
//! A source that changes size, a camera that goes inactive or disappears, five seconds without a
//! frame, or a stop request each end the recording early. What was recorded is kept and reported
//! as success, with [`SavedVideo::ended`] saying why. Ending before any frame is an error.
//!
//! Despawning a request, or exiting the app, cancels the recording and deletes the partial file
//! without reporting. Once the writer has started moving the finished file into place, cancel
//! loses: the file appears. A hard kill of the game can leave an `.ename-video-*` temp file.

mod encoder;
mod length;
#[allow(dead_code, reason = "used by the capture pass that follows")]
mod nv12;

use std::{
    collections::BTreeMap,
    ffi::OsString,
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        mpsc::{SyncSender, TrySendError, sync_channel},
    },
    thread::JoinHandle,
    time::Duration,
};

use bevy::{
    camera::RenderTarget, prelude::*, render::render_resource::TextureFormat, window::PrimaryWindow,
};

pub use crate::readback::CaptureSource;
use crate::readback::{
    FrameReadBack, Limit, ReadbackQueue, ReadbackSystems, ReadbackTag, SourceError,
    add_readback_plugin, resolve_source,
};
use encoder::{
    Container, EncoderSpec, End, Frame, WriterJob, WriterShared, encoded_rect, shutdown_writers,
    spawn_writer,
};
pub use length::{VideoLength, parse_timecode};

/// Frames queued for the writer before more are dropped. About 66 MB at 1080p.
const FRAME_QUEUE: usize = 8;
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
/// How long a request with [`RetainVideoOutcome`] stays alive after it finishes.
const OUTCOME_TTL: Duration = Duration::from_secs(5 * 60);
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

/// Keeps a finished request alive for 5 minutes with a [`VideoOutcome`], for callers that poll.
#[derive(Component, Default)]
pub struct RetainVideoOutcome;

/// Where a request is, for pollers. Written only when a value changes.
#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoProgress {
    pub phase: VideoPhase,
    /// Frames accepted so far.
    pub frames: u32,
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
    /// Finished; the outcome is in [`VideoOutcome`].
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

/// The finished result, kept on a request with [`RetainVideoOutcome`].
#[derive(Component, Clone, Debug)]
pub struct VideoOutcome(pub Result<SavedVideo, VideoError>);

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
        add_readback_plugin(app);
        app.insert_resource(VideoSettings {
            forced_encoder: std::env::var("ENAME_VIDEO_ENCODER")
                .ok()
                .filter(|name| !name.is_empty()),
            ..default()
        })
        .init_resource::<ActiveWriters>()
        .add_systems(
            Update,
            (
                advance_starting,
                finish_recordings,
                expire_recordings,
                update_progress,
            )
                .chain(),
        )
        .add_systems(
            Last,
            (
                (start_recordings, request_frames)
                    .chain()
                    .in_set(ReadbackSystems::Request),
                shutdown_on_exit,
            ),
        )
        .add_observer(receive_frame)
        .add_observer(stop_recording);
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
        frames: SyncSender<Frame>,
        since: Duration,
        stop: bool,
    },
    Recording(Box<Recorder>),
    /// The channel is closed; the writer encodes the tail and closes the file by `deadline`.
    Finalizing {
        writer: WriterHandle,
        deadline: Duration,
        dropped: u32,
    },
    Done {
        expires: Duration,
    },
}

struct Recorder {
    writer: WriterHandle,
    frames: SyncSender<Frame>,
    length: VideoLength,
    frame_period: Duration,
    /// The first request's crop size, to spot a resize before reading back.
    first_crop: Option<Option<UVec2>>,
    /// The first accepted frame's encoded size and format.
    first: Option<(UVec2, TextureFormat)>,
    /// Real time of the first request. Frame stamps are relative to it.
    t0: Option<Duration>,
    last_accepted_at: Duration,
    next_seq: u64,
    /// Requested and not yet delivered: seq to request time. Bookkeeping for draining only.
    pending: BTreeMap<u64, Duration>,
    last_accepted: Option<(u64, Duration)>,
    /// Time mode: the request at or after `t0 + length`. Later deliveries are ignored.
    cutoff_seq: Option<u64>,
    accepted: u32,
    dropped: u32,
    /// Why and when it ended. Requests stop; deliveries up to the last request still count.
    end: Option<(VideoEnd, Duration)>,
}

impl Recorder {
    fn end(&mut self, reason: VideoEnd, now: Duration) {
        if self.end.is_none() {
            self.end = Some((reason, now));
        }
    }
}

fn start_recordings(
    mut commands: Commands,
    requests: Query<(Entity, &VideoRequest, Has<RetainVideoOutcome>), Without<RecordingStage>>,
    cameras: Query<(&Camera, &RenderTarget)>,
    primary_window: Query<Entity, With<PrimaryWindow>>,
    settings: Res<VideoSettings>,
    mut writers: ResMut<ActiveWriters>,
    time: Res<Time<Real>>,
) {
    let now = time.elapsed();
    let primary_window = primary_window.single().ok();
    for (entity, request, retain) in &requests {
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
                commands.entity(entity).try_insert(RecordingStage::Done {
                    expires: now + OUTCOME_TTL,
                });
                report(&mut commands, entity, retain, Err(err));
                continue;
            }
        };

        let (sender, receiver) = sync_channel(FRAME_QUEUE);
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
                frames: sender,
                since: now,
                stop: false,
            });
    }
}

/// Moves requests whose writer has an encoder to recording. The stall clock starts here, so a
/// slow probe never reads as a stall.
fn advance_starting(
    mut commands: Commands,
    mut requests: Query<(
        Entity,
        &VideoRequest,
        &mut RecordingStage,
        Has<RetainVideoOutcome>,
    )>,
    settings: Res<VideoSettings>,
    time: Res<Time<Real>>,
) {
    let now = time.elapsed();
    for (entity, request, mut stage, retain) in &mut requests {
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
            *stage = RecordingStage::Done {
                expires: now + OUTCOME_TTL,
            };
            report(&mut commands, entity, retain, Err(err));
            continue;
        }
        if writer.0.ready.get().is_none() {
            continue;
        }
        let RecordingStage::Starting { writer, frames, .. } = std::mem::replace(
            &mut *stage,
            RecordingStage::Done {
                expires: Duration::ZERO,
            },
        ) else {
            unreachable!();
        };
        *stage = RecordingStage::Recording(Box::new(Recorder {
            writer,
            frames,
            length: request.length,
            frame_period: Duration::from_secs_f64(1.0 / f64::from(request.frame_rate.get())),
            first_crop: None,
            first: None,
            t0: None,
            last_accepted_at: now,
            next_seq: 0,
            pending: BTreeMap::new(),
            last_accepted: None,
            cutoff_seq: None,
            accepted: 0,
            dropped: 0,
            end: None,
        }));
    }
}

fn request_frames(
    mut requests: Query<(Entity, &VideoRequest, &mut RecordingStage)>,
    cameras: Query<(&Camera, &RenderTarget)>,
    primary_window: Query<Entity, With<PrimaryWindow>>,
    mut queue: ResMut<ReadbackQueue>,
    settings: Res<VideoSettings>,
    time: Res<Time<Real>>,
) {
    let now = time.elapsed();
    let primary_window = primary_window.single().ok();
    for (entity, request, mut stage) in &mut requests {
        let RecordingStage::Recording(recorder) = &mut *stage else {
            continue;
        };
        // Only stops this recording waiting; the GPU slot is the queue's to give back.
        let before = recorder.pending.len();
        recorder
            .pending
            .retain(|_, requested| now.saturating_sub(*requested) < settings.stall_timeout);
        recorder.dropped += (before - recorder.pending.len()) as u32;
        if recorder.end.is_some() {
            continue;
        }

        let source = match resolve_source(request.source, &cameras, primary_window) {
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
        let crop_size = source.crop.map(|crop| crop.size());
        match recorder.first_crop {
            None => recorder.first_crop = Some(crop_size),
            Some(first) if first != crop_size => {
                recorder.end(VideoEnd::SourceResized, now);
                continue;
            }
            Some(_) => {}
        }
        if now.saturating_sub(recorder.last_accepted_at) > settings.stall_timeout {
            recorder.end(VideoEnd::Stalled, now);
            continue;
        }

        let seq = recorder.next_seq;
        let tag = ReadbackTag {
            crop: source.crop,
            seq,
            requested_at: now,
        };
        if !queue.request(&source, entity, tag, Limit::Bounded) {
            recorder.dropped += 1;
            continue;
        }
        recorder.next_seq += 1;
        recorder.pending.insert(seq, now);
        let t0 = *recorder.t0.get_or_insert(now);
        if let VideoLength::Time(length) = recorder.length
            && now >= t0 + length
        {
            recorder.cutoff_seq = Some(seq);
            recorder.end(VideoEnd::Completed, now);
        }
    }
}

/// Hands one delivered frame to the writer. Never blocks: a full queue drops the frame.
fn receive_frame(
    frame: On<FrameReadBack>,
    mut requests: Query<&mut RecordingStage>,
    time: Res<Time<Real>>,
) {
    let Ok(mut stage) = requests.get_mut(frame.entity) else {
        return;
    };
    let RecordingStage::Recording(recorder) = &mut *stage else {
        return;
    };
    let now = time.elapsed();
    let seq = frame.tag.seq;
    recorder.pending.remove(&seq);
    if let VideoLength::Frames(limit) = recorder.length
        && recorder.accepted >= limit.get()
    {
        return;
    }
    if recorder.cutoff_seq.is_some_and(|cutoff| seq > cutoff) {
        return;
    }
    if recorder.last_accepted.is_some_and(|(last, _)| seq <= last) {
        recorder.dropped += 1;
        return;
    }
    let format = frame.image.texture_descriptor.format;
    let size = encoded_rect(frame.image.size(), frame.tag.crop).map(|rect| rect.size());
    match (recorder.first, size) {
        (None, Ok(size)) => recorder.first = Some((size, format)),
        (Some(first), Ok(size)) if first == (size, format) => {}
        // The first frame's own errors are the writer's to report.
        (None, Err(_)) => {}
        (Some(_), _) => {
            recorder.dropped += 1;
            recorder.end(VideoEnd::SourceResized, now);
            return;
        }
    }
    let Some(t0) = recorder.t0 else {
        return;
    };
    let stamp = frame.tag.requested_at.saturating_sub(t0);
    match recorder.frames.try_send(Frame {
        image: frame.image.clone(),
        crop: frame.tag.crop,
        stamp,
    }) {
        Ok(()) => {
            recorder.accepted += 1;
            recorder.last_accepted = Some((seq, stamp));
            recorder.last_accepted_at = now;
            if let VideoLength::Frames(limit) = recorder.length
                && recorder.accepted >= limit.get()
            {
                recorder.end(VideoEnd::Completed, now);
            }
        }
        Err(TrySendError::Full(_)) => recorder.dropped += 1,
        // The writer has stopped; `finish_recordings` reports why.
        Err(TrySendError::Disconnected(_)) => {}
    }
}

/// A request that has not started yet finishes right here, so a stop sent in the same frame as
/// the request is not lost.
fn stop_recording(
    stop: On<VideoStopRequested>,
    mut requests: Query<(Option<&mut RecordingStage>, Has<RetainVideoOutcome>), With<VideoRequest>>,
    time: Res<Time<Real>>,
    mut commands: Commands,
) {
    let Ok((stage, retain)) = requests.get_mut(stop.entity) else {
        return;
    };
    let Some(mut stage) = stage else {
        commands
            .entity(stop.entity)
            .try_insert(RecordingStage::Done {
                expires: time.elapsed() + OUTCOME_TTL,
            });
        report(
            &mut commands,
            stop.entity,
            retain,
            Err(VideoError::NoFrames(VideoEnd::Stopped)),
        );
        return;
    };
    match &mut *stage {
        RecordingStage::Starting { stop, .. } => *stop = true,
        RecordingStage::Recording(recorder) => recorder.end(VideoEnd::Stopped, time.elapsed()),
        RecordingStage::Finalizing { .. } | RecordingStage::Done { .. } => {}
    }
}

fn finish_recordings(
    mut commands: Commands,
    mut requests: Query<(Entity, &mut RecordingStage, Has<RetainVideoOutcome>)>,
    mut writers: ResMut<ActiveWriters>,
    settings: Res<VideoSettings>,
    time: Res<Time<Real>>,
) {
    let now = time.elapsed();
    for (entity, mut stage, retain) in &mut requests {
        let mut closing = None;
        let outcome = match &mut *stage {
            RecordingStage::Recording(recorder) => {
                if let Some(result) = recorder.writer.0.result.get() {
                    // The writer stopped on its own while frames were still coming: an error.
                    Some(result.clone())
                } else if recorder.writer.0.stuck_for() > settings.write_timeout {
                    Some(Err(stalled_ffmpeg("ffmpeg stopped reading frames")))
                } else {
                    closing = recorder.end.and_then(|(reason, ended_at)| {
                        (recorder.pending.is_empty() || now >= ended_at + settings.drain_timeout)
                            .then_some(reason)
                    });
                    None
                }
            }
            RecordingStage::Finalizing {
                writer,
                deadline,
                dropped,
            } => match writer.0.result.get() {
                Some(result) => Some(result.clone().map(|mut saved| {
                    saved.dropped_frames = *dropped;
                    saved
                })),
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
            *stage = RecordingStage::Done {
                expires: now + OUTCOME_TTL,
            };
            report(&mut commands, entity, retain, result);
        }
    }
    writers.0.retain(|(_, thread)| !thread.is_finished());
}

/// Closes the channel and gives the writer its end.
fn close(stage: &mut RecordingStage, reason: VideoEnd, deadline: Duration) {
    let RecordingStage::Recording(recorder) = std::mem::replace(
        stage,
        RecordingStage::Done {
            expires: Duration::ZERO,
        },
    ) else {
        return;
    };
    let Recorder {
        writer,
        frames,
        frame_period,
        last_accepted,
        dropped,
        ..
    } = *recorder;
    let cutoff = match reason {
        VideoEnd::Completed => None,
        _ => Some(
            last_accepted
                .map(|(_, stamp)| stamp + frame_period)
                .unwrap_or_default(),
        ),
    };
    writer.0.end(End { reason, cutoff });
    drop(frames);
    *stage = RecordingStage::Finalizing {
        writer,
        deadline,
        dropped,
    };
}

fn stalled_ffmpeg(status: &str) -> VideoError {
    VideoError::Ffmpeg {
        status: status.to_owned(),
        stderr: String::new(),
    }
}

fn expire_recordings(
    mut commands: Commands,
    requests: Query<(Entity, &RecordingStage)>,
    time: Res<Time<Real>>,
) {
    let now = time.elapsed();
    for (entity, stage) in &requests {
        if let RecordingStage::Done { expires } = stage
            && now >= *expires
        {
            commands.entity(entity).try_despawn();
        }
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
                frames: recorder.accepted,
                dropped_frames: recorder.dropped,
                elapsed: recorder.t0.map(|t0| now - t0).unwrap_or_default(),
            },
            RecordingStage::Finalizing { .. } => VideoProgress {
                phase: VideoPhase::Finalizing,
                ..progress.clone()
            },
            RecordingStage::Done { .. } => VideoProgress {
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

/// Publishes a finished request; the caller has already moved it to `Done`.
fn report(
    commands: &mut Commands,
    entity: Entity,
    retain: bool,
    result: Result<SavedVideo, VideoError>,
) {
    if retain {
        commands
            .entity(entity)
            .try_insert(VideoOutcome(result.clone()));
    }
    commands.trigger(VideoFinished { entity, result });
    if !retain {
        commands.entity(entity).try_despawn();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        readback::ReadbackFanOut,
        screenshot::{ScreenshotPlugin, ScreenshotRequest},
    };
    use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
    use encoder::tests::{fake_exited, fake_ffmpeg, lock_processes, test_image};
    use std::time::Instant;

    #[derive(Resource, Default)]
    struct Finished(Vec<(Entity, Result<SavedVideo, VideoError>)>);

    const CAT: &str = "exec cat > \"$out\"";

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
        app.world_mut().spawn((Window::default(), PrimaryWindow));
        app
    }

    fn record(app: &mut App, path: PathBuf, length: VideoLength) -> Entity {
        app.world_mut()
            .spawn((
                VideoRequest::new(CaptureSource::PrimaryWindow, path, length),
                RetainVideoOutcome,
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

    /// Delivers every readback spawned so far, in one go, then forgets them.
    fn deliver_all(app: &mut App, size: u32) -> usize {
        let screenshots: Vec<Entity> = app
            .world_mut()
            .query_filtered::<Entity, (With<Screenshot>, With<ReadbackFanOut>)>()
            .iter(app.world())
            .collect();
        for &screenshot in &screenshots {
            app.world_mut().trigger(ScreenshotCaptured {
                entity: screenshot,
                image: test_image(size, size, TextureFormat::Rgba8UnormSrgb),
            });
            app.world_mut().despawn(screenshot);
        }
        app.world_mut().flush();
        screenshots.len()
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
    fn frames_mode_accepts_exactly_n_from_one_batch() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path(), "exit 0", CAT, |_| {});
        let path = dir.path().join("a.mp4");
        let entity = record(&mut app, path.clone(), frames(2));
        recording(&mut app, entity);
        for _ in 0..4 {
            app.update();
        }
        assert_eq!(
            deliver_all(&mut app, 8),
            crate::readback::MAX_IN_FLIGHT as usize
        );
        let saved = finished(&mut app).unwrap();
        assert_eq!((saved.frames, saved.ended), (2, VideoEnd::Completed));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 2 * 8 * 8 * 4);
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
        assert!(
            app.world_mut()
                .query::<&Screenshot>()
                .iter(app.world())
                .next()
                .is_none()
        );
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
        for _ in 0..2 {
            app.update();
            std::thread::sleep(Duration::from_millis(20));
            deliver_all(&mut app, 8);
        }
        let recorded = started.elapsed();
        app.world_mut().trigger(VideoStopRequested { entity });
        let saved = finished(&mut app).unwrap();
        assert_eq!(saved.ended, VideoEnd::Stopped);
        // The ticks up to the last delivered stamp plus one period: bounded by the wall time the
        // frames were requested over, whatever the machine's load. Nowhere near an hour.
        let bound = (recorded.as_secs_f64() * 60.0).ceil() as u32 + 2;
        assert!(
            (1..=bound).contains(&saved.frames),
            "{} frames in {recorded:?}",
            saved.frames
        );
        let bytes = std::fs::metadata(&path).unwrap().len();
        assert_eq!(bytes, u64::from(saved.frames) * 8 * 8 * 4);
    }

    #[test]
    fn a_size_change_ends_the_recording_and_keeps_it() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path(), "exit 0", CAT, |_| {});
        let entity = record(&mut app, dir.path().join("a.mp4"), frames(10));
        recording(&mut app, entity);
        app.update();
        deliver_all(&mut app, 8);
        app.update();
        deliver_all(&mut app, 10);
        let saved = finished(&mut app).unwrap();
        // Every 8x8 frame before the change is kept.
        assert_eq!(saved.ended, VideoEnd::SourceResized);
        assert!(saved.frames >= 1 && saved.size == UVec2::splat(8));
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
            assert!(
                app.world_mut()
                    .query::<&Screenshot>()
                    .iter(app.world())
                    .next()
                    .is_none()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        recording(&mut app, entity);
        app.update();
        deliver_all(&mut app, 8);
        assert_eq!(finished(&mut app).unwrap().frames, 1);
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
        // 64 KiB frames: the pipe fills and the writer blocks in a write.
        for _ in 0..6 {
            app.update();
            deliver_all(&mut app, 128);
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
        update_until(&mut app, "the watchdog", |app| {
            deliver_all(app, 128);
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
        update_until(&mut app, "the finalize deadline", |app| {
            deliver_all(app, 128);
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

    #[test]
    fn a_recording_and_a_screenshot_share_one_readback() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path(), "exit 0", CAT, |_| {});
        app.add_plugins(ScreenshotPlugin);
        let entity = record(&mut app, dir.path().join("a.mp4"), frames(5));
        recording(&mut app, entity);
        let shot = app
            .world_mut()
            .spawn(ScreenshotRequest {
                source: CaptureSource::PrimaryWindow,
                path: dir.path().join("a.png"),
            })
            .id();
        app.update();
        let fan_outs: Vec<Vec<Entity>> = app
            .world_mut()
            .query::<&ReadbackFanOut>()
            .iter(app.world())
            .map(|fan_out| fan_out.members.iter().map(|(entity, _)| *entity).collect())
            .collect();
        assert!(fan_outs.contains(&vec![entity, shot]) || fan_outs.contains(&vec![shot, entity]));
    }

    #[test]
    fn an_expired_readback_keeps_its_slot() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path(), "exit 0", CAT, |settings| {
            settings.stall_timeout = Duration::from_millis(200);
        });
        let entity = record(&mut app, dir.path().join("a.mp4"), frames(100));
        recording(&mut app, entity);
        for _ in 0..5 {
            app.update();
        }
        let readbacks = |app: &mut App| {
            app.world_mut()
                .query::<&ReadbackFanOut>()
                .iter(app.world())
                .count()
        };
        assert_eq!(readbacks(&mut app), crate::readback::MAX_IN_FLIGHT as usize);
        std::thread::sleep(Duration::from_millis(300));
        // The recording gives up on them and stalls, but the GPU slots stay taken until Bevy
        // delivers or drops them, or the 10 s reclaim time passes.
        assert_eq!(
            finished(&mut app),
            Err(VideoError::TimedOut(Duration::from_millis(200)))
        );
        assert_eq!(readbacks(&mut app), crate::readback::MAX_IN_FLIGHT as usize);
        let window = app
            .world_mut()
            .query_filtered::<Entity, With<PrimaryWindow>>()
            .single(app.world())
            .unwrap();
        let key = RenderTarget::Window(bevy::window::WindowRef::Primary)
            .normalize(Some(window))
            .unwrap();
        let now = app.world().resource::<Time<Real>>().elapsed();
        assert_eq!(
            app.world()
                .resource::<ReadbackQueue>()
                .outstanding(&key, now),
            crate::readback::MAX_IN_FLIGHT
        );
    }
}
