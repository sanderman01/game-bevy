//! The writer thread: picks an encoder, then streams cropped frames to an `ffmpeg` child process.
//!
//! The writer blocks in only three places, and each wakes on cancel: `recv_timeout` checks the
//! cancel flag every 100 ms, a pipe write breaks when the child is killed, and waiting on the
//! child is a `try_wait` loop that checks the flag. The child sits in [`WriterShared::child`],
//! locked only to register, poll and kill it, never across I/O.

use std::{
    ffi::OsString,
    io::{ErrorKind, Write as _},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU8, AtomicU64, Ordering},
        mpsc::{Receiver, RecvTimeoutError},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use bevy::{prelude::*, render::render_resource::TextureFormat};

use super::{SavedVideo, VideoEnd, VideoError, VideoLength};

/// One encoder ffmpeg may have, with the arguments it needs.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct EncoderSpec {
    pub name: &'static str,
    /// Global options, such as a hardware device.
    input_args: &'static [&'static str],
    /// The software pixel format the encoder takes, before any upload.
    pix_fmt: &'static str,
    hw_upload: bool,
    quality_args: &'static [&'static str],
}

const VAAPI_DEVICE: &[&str] = &["-init_hw_device", "vaapi=va", "-filter_hw_device", "va"];

pub(crate) const ENCODERS: &[EncoderSpec] = &[
    EncoderSpec {
        name: "h264_vaapi",
        input_args: VAAPI_DEVICE,
        pix_fmt: "nv12",
        hw_upload: true,
        quality_args: &["-rc_mode", "CQP", "-qp", "20"],
    },
    EncoderSpec {
        name: "h264_nvenc",
        input_args: &[],
        pix_fmt: "nv12",
        hw_upload: false,
        quality_args: &["-preset", "p4", "-rc", "constqp", "-qp", "20"],
    },
    EncoderSpec {
        name: "h264_amf",
        input_args: &[],
        pix_fmt: "nv12",
        hw_upload: false,
        quality_args: &["-rc", "cqp", "-qp_i", "20", "-qp_p", "20"],
    },
    EncoderSpec {
        name: "h264_qsv",
        input_args: &[],
        pix_fmt: "nv12",
        hw_upload: false,
        quality_args: &["-global_quality", "20"],
    },
    EncoderSpec {
        name: "h264_videotoolbox",
        input_args: &[],
        pix_fmt: "nv12",
        hw_upload: false,
        quality_args: &["-q:v", "65"],
    },
    EncoderSpec {
        name: "libx264",
        input_args: &[],
        pix_fmt: "yuv420p",
        hw_upload: false,
        quality_args: &["-preset", "veryfast", "-crf", "20"],
    },
];

/// Encoders to try, best first.
fn candidates() -> &'static [&'static str] {
    if cfg!(target_os = "linux") {
        &["h264_vaapi", "h264_nvenc", "h264_qsv", "libx264"]
    } else if cfg!(target_os = "windows") {
        &["h264_nvenc", "h264_amf", "h264_qsv", "libx264"]
    } else if cfg!(target_os = "macos") {
        &["h264_videotoolbox", "libx264"]
    } else {
        &["libx264"]
    }
}

fn spec(name: &str) -> Option<&'static EncoderSpec> {
    ENCODERS.iter().find(|spec| spec.name == name)
}

/// The output container, chosen by the path's extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Container {
    Mp4,
    Matroska,
}

impl Container {
    pub(crate) fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?;
        if extension.eq_ignore_ascii_case("mp4") {
            Some(Self::Mp4)
        } else if extension.eq_ignore_ascii_case("mkv") {
            Some(Self::Matroska)
        } else {
            None
        }
    }

    fn format(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Matroska => "matroska",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Mp4 => ".mp4",
            Self::Matroska => ".mkv",
        }
    }
}

/// Converts the matrix and range only, then tags the samples with what they are: sRGB transfer,
/// BT.709 primaries and matrix. `setparams`, because output-side colour options are lost after
/// `scale`.
fn video_filter(spec: &EncoderSpec) -> String {
    let mut filter = format!(
        "scale=out_color_matrix=bt709:out_range=tv,format={},\
         setparams=color_primaries=bt709:color_trc=iec61966-2-1:colorspace=bt709:range=tv",
        spec.pix_fmt
    );
    if spec.hw_upload {
        filter.push_str(",hwupload");
    }
    filter
}

fn probe_args(spec: &EncoderSpec) -> Vec<String> {
    let mut args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "color=black:s=256x256:r=30",
        "-frames:v",
        "3",
    ]
    .map(String::from)
    .into();
    args.extend(spec.input_args.iter().map(|arg| arg.to_string()));
    args.extend([
        "-vf".into(),
        video_filter(spec),
        "-c:v".into(),
        spec.name.into(),
    ]);
    args.extend(spec.quality_args.iter().map(|arg| arg.to_string()));
    args.extend(["-f", "null", "-"].map(String::from));
    args
}

/// The arguments for one recording, input from stdin.
pub(crate) fn encode_args(
    spec: &EncoderSpec,
    format: TextureFormat,
    size: UVec2,
    frame_rate: u32,
    container: Container,
    output: &Path,
) -> Vec<OsString> {
    let pix_fmt = match format {
        TextureFormat::Bgra8UnormSrgb => "bgra",
        _ => "rgba",
    };
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |arg: &str| args.push(arg.into());
    for arg in [
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "rawvideo",
        "-pix_fmt",
        pix_fmt,
        "-s",
        &format!("{}x{}", size.x, size.y),
        "-framerate",
        &frame_rate.to_string(),
        "-i",
        "pipe:0",
    ] {
        push(arg);
    }
    spec.input_args.iter().for_each(|arg| push(arg));
    for arg in ["-vf", &video_filter(spec), "-c:v", spec.name] {
        push(arg);
    }
    spec.quality_args.iter().for_each(|arg| push(arg));
    for arg in [
        "-g",
        &(2 * frame_rate).to_string(),
        "-an",
        "-f",
        container.format(),
    ] {
        push(arg);
    }
    args.push(output.into());
    args
}

/// Output ticks to fill before a frame at `stamp`: those from `next` up to the tick nearest
/// `stamp`, which the new frame takes, and never past `limit`. Tick `k` is at `k / fps`.
/// Nearest, not next: a game running at the output rate has stamps that jitter around the ticks,
/// and rounding up would flip frames between duplicated and dropped.
pub(crate) fn ticks_until(
    stamp: Duration,
    fps: u32,
    next: u64,
    limit: u64,
) -> std::ops::Range<u64> {
    const NANOS: u128 = 1_000_000_000;
    let end = (stamp.as_nanos() * fps as u128 + NANOS / 2) / NANOS;
    let end = (end.min(limit as u128)) as u64;
    next..end.max(next)
}

/// The rect of `image` a frame keeps: its crop clamped to the image, rounded down to even sizes.
pub(crate) fn encoded_rect(image_size: UVec2, crop: Option<URect>) -> Result<URect, VideoError> {
    let bounds = URect::from_corners(UVec2::ZERO, image_size);
    let area = match crop {
        Some(crop) => {
            let area = crop.intersect(bounds);
            if area.is_empty() {
                return Err(VideoError::EmptyCrop {
                    crop,
                    size: image_size,
                });
            }
            area
        }
        None => bounds,
    };
    let size = area.size() & !UVec2::ONE;
    if size.x == 0 || size.y == 0 {
        return Err(VideoError::TooSmall { size: area.size() });
    }
    Ok(URect::from_corners(area.min, area.min + size))
}

/// Copies `rect` of a 4-byte-per-pixel image into `out`, packed.
fn copy_rect(image: &Image, rect: URect, out: &mut Vec<u8>) {
    out.clear();
    let Some(data) = image.data.as_deref() else {
        return;
    };
    let stride = image.width() as usize * 4;
    let (x, width) = (rect.min.x as usize * 4, rect.width() as usize * 4);
    for y in rect.min.y..rect.max.y {
        let start = y as usize * stride + x;
        out.extend_from_slice(&data[start..start + width]);
    }
}

/// One frame for the writer. `stamp` is relative to the recording's first request.
pub(crate) struct Frame {
    pub image: Arc<Image>,
    pub crop: Option<URect>,
    pub stamp: Duration,
}

/// How a recording ends. `cutoff` (relative to the first request) bounds the time-mode ticks for
/// an early end; `None` pads to the full length.
#[derive(Clone, Copy, Debug)]
pub(crate) struct End {
    pub reason: VideoEnd,
    pub cutoff: Option<Duration>,
}

const RUNNING: u8 = 0;
const CANCELLED: u8 = 1;
const PUBLISHING: u8 = 2;

/// Shared by the main world and one writer thread.
pub(crate) struct WriterShared {
    /// RUNNING -> CANCELLED (cancel) or RUNNING -> PUBLISHING (writer, just before the rename),
    /// both by compare-exchange, so cancel and publish cannot both win.
    state: AtomicU8,
    /// The child running now, probe or encoder, with its pipes already taken.
    child: Mutex<Option<Child>>,
    end: OnceLock<End>,
    epoch: Instant,
    /// Milliseconds after `epoch` plus one when the writer entered a blocking write or wait;
    /// 0 when it is not in one.
    busy_since: AtomicU64,
    /// The encoder chosen, or why there is none.
    pub ready: OnceLock<Result<&'static str, VideoError>>,
    /// The one terminal result. Not set for a cancelled writer: nobody reads it.
    pub result: OnceLock<Result<SavedVideo, VideoError>>,
}

impl WriterShared {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(RUNNING),
            child: Mutex::new(None),
            end: OnceLock::new(),
            epoch: Instant::now(),
            busy_since: AtomicU64::new(0),
            ready: OnceLock::new(),
            result: OnceLock::new(),
        }
    }

    /// Cancels the writer and kills its child. Never waits. Loses to a writer already publishing.
    pub(crate) fn cancel(&self) {
        let _ = self
            .state
            .compare_exchange(RUNNING, CANCELLED, Ordering::SeqCst, Ordering::SeqCst);
        if self.cancelled() {
            self.kill_child();
        }
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.state.load(Ordering::SeqCst) == CANCELLED
    }

    pub(crate) fn end(&self, end: End) {
        let _ = self.end.set(end);
    }

    /// How long the writer has been inside one blocking write or wait.
    pub(crate) fn stuck_for(&self) -> Duration {
        match self.busy_since.load(Ordering::SeqCst) {
            0 => Duration::ZERO,
            since => self
                .epoch
                .elapsed()
                .saturating_sub(Duration::from_millis(since - 1)),
        }
    }

    fn kill_child(&self) {
        if let Some(child) = self.lock_child().as_mut() {
            let _ = child.kill();
        }
    }

    fn lock_child(&self) -> std::sync::MutexGuard<'_, Option<Child>> {
        self.child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn set_busy(&self, busy: bool) {
        let value = if busy {
            self.epoch.elapsed().as_millis() as u64 + 1
        } else {
            0
        };
        self.busy_since.store(value, Ordering::SeqCst);
    }
}

/// What a writer is told to do.
pub(crate) struct WriterJob {
    pub path: PathBuf,
    pub container: Container,
    pub length: VideoLength,
    pub frame_rate: u32,
    pub frames: Receiver<Frame>,
    pub ffmpeg: OsString,
    pub forced_encoder: Option<String>,
    pub probe_timeout: Duration,
    /// Reported in [`VideoError::TimedOut`] when a recording stalls before its first frame.
    pub stall_timeout: Duration,
    /// Per app, so tests with fake programs do not share a probe result.
    pub encoder_cache: Arc<OnceLock<&'static EncoderSpec>>,
}

/// Starts a writer thread.
pub(crate) fn spawn_writer(job: WriterJob) -> (Arc<WriterShared>, JoinHandle<()>) {
    let shared = Arc::new(WriterShared::new());
    let thread_shared = shared.clone();
    let thread = std::thread::Builder::new()
        .name("video-writer".into())
        .spawn(move || run(job, &thread_shared))
        .expect("could not spawn the video writer thread");
    (shared, thread)
}

/// Cancels every writer, then waits up to `bound` for them all, holding no lock. Any still
/// running are detached.
pub(crate) fn shutdown_writers(writers: Vec<(Arc<WriterShared>, JoinHandle<()>)>, bound: Duration) {
    for (shared, _) in &writers {
        shared.cancel();
    }
    let deadline = Instant::now() + bound;
    let mut remaining = writers;
    while !remaining.is_empty() && Instant::now() < deadline {
        let (finished, running): (Vec<_>, Vec<_>) = remaining
            .into_iter()
            .partition(|(_, thread)| thread.is_finished());
        for (_, thread) in finished {
            let _ = thread.join();
        }
        remaining = running;
        if !remaining.is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    if !remaining.is_empty() {
        warn!(
            "{} video writer thread(s) did not stop within {bound:?}; detaching them",
            remaining.len()
        );
    }
}

/// Why the writer stopped without a result to report.
enum Stop {
    Cancelled,
    Failed(VideoError),
}

impl From<VideoError> for Stop {
    fn from(error: VideoError) -> Self {
        Self::Failed(error)
    }
}

fn run(job: WriterJob, shared: &WriterShared) {
    let mut session = Session {
        shared,
        stdin: None,
        stderr: None,
    };
    let result = write(&job, shared, &mut session);
    let stderr = session.shutdown();
    match result {
        Ok(saved) => {
            let _ = shared.result.set(Ok(saved));
        }
        Err(Stop::Failed(VideoError::Ffmpeg { status, .. })) => {
            let _ = shared
                .result
                .set(Err(VideoError::Ffmpeg { status, stderr }));
        }
        Err(Stop::Failed(error)) => {
            let _ = shared.result.set(Err(error));
        }
        Err(Stop::Cancelled) => {}
    }
}

/// The running child's taken pipes. [`Session::shutdown`] kills and reaps the child.
struct Session<'a> {
    shared: &'a WriterShared,
    stdin: Option<ChildStdin>,
    stderr: Option<JoinHandle<Vec<u8>>>,
}

impl Session<'_> {
    /// Registers `child` (pipes already taken), then re-checks cancel, so a cancel that ran
    /// between spawn and here still kills it.
    fn register(&mut self, child: Child) -> Result<(), Stop> {
        *self.shared.lock_child() = Some(child);
        if self.shared.cancelled() {
            self.shared.kill_child();
            return Err(Stop::Cancelled);
        }
        Ok(())
    }

    /// Waits for the registered child to exit, waking on cancel or `deadline`.
    fn wait(&self, deadline: Option<Instant>) -> Result<ExitStatus, Stop> {
        self.shared.set_busy(true);
        let result = loop {
            let status = match self.shared.lock_child().as_mut() {
                Some(child) => child.try_wait(),
                None => break Err(Stop::Cancelled),
            };
            match status {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => {}
                Err(err) => break Err(Stop::Failed(ffmpeg_error(err.to_string()))),
            }
            if self.shared.cancelled() {
                break Err(Stop::Cancelled);
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                self.shared.kill_child();
                break Err(Stop::Failed(ffmpeg_error("timed out".into())));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        self.shared.set_busy(false);
        if self.shared.cancelled() {
            return Err(Stop::Cancelled);
        }
        result
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), Stop> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(Stop::Cancelled);
        };
        self.shared.set_busy(true);
        let result = stdin.write_all(bytes);
        self.shared.set_busy(false);
        match result {
            Ok(()) => Ok(()),
            Err(_) if self.shared.cancelled() => Err(Stop::Cancelled),
            Err(err) => Err(Stop::Failed(ffmpeg_error(format!(
                "ffmpeg stopped reading frames: {err}"
            )))),
        }
    }

    /// Kills and reaps the child if there is one, joins the stderr reader, and returns the tail
    /// of what ffmpeg printed. Safe to call twice.
    fn shutdown(&mut self) -> String {
        self.stdin = None;
        // Out of the slot first, so the lock is not held while reaping.
        let child = self.shared.lock_child().take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
        let tail = self
            .stderr
            .take()
            .and_then(|thread| thread.join().ok())
            .unwrap_or_default();
        String::from_utf8_lossy(&tail).trim().to_owned()
    }
}

fn ffmpeg_error(status: String) -> VideoError {
    VideoError::Ffmpeg {
        status,
        stderr: String::new(),
    }
}

fn spawn_error(err: std::io::Error) -> VideoError {
    if err.kind() == ErrorKind::NotFound {
        VideoError::FfmpegMissing
    } else {
        ffmpeg_error(format!("could not start ffmpeg: {err}"))
    }
}

/// Keeps the last 4 KiB of a pipe.
fn drain_tail(mut pipe: impl std::io::Read) -> Vec<u8> {
    const KEEP: usize = 4096;
    let mut tail = Vec::new();
    let mut chunk = [0; 1024];
    while let Ok(read) = pipe.read(&mut chunk) {
        if read == 0 {
            break;
        }
        tail.extend_from_slice(&chunk[..read]);
        if tail.len() > KEEP {
            tail.drain(..tail.len() - KEEP);
        }
    }
    tail
}

/// Finds a working encoder, using and filling the cache.
fn pick_encoder(job: &WriterJob, session: &mut Session) -> Result<&'static EncoderSpec, Stop> {
    if let Some(found) = job.encoder_cache.get() {
        return Ok(found);
    }
    let names: Vec<&str> = match &job.forced_encoder {
        Some(name) => vec![name.as_str()],
        None => candidates().to_vec(),
    };
    let mut tried = Vec::new();
    for name in names {
        tried.push(name.to_owned());
        let Some(spec) = spec(name) else {
            let known: Vec<_> = ENCODERS.iter().map(|spec| spec.name).collect();
            return Err(VideoError::NoEncoder {
                tried: vec![format!("{name} (unknown; known: {})", known.join(", "))],
            }
            .into());
        };
        let child = Command::new(&job.ffmpeg)
            .args(probe_args(spec))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(spawn_error)?;
        session.register(child)?;
        let status = session.wait(Some(Instant::now() + job.probe_timeout));
        session.shutdown();
        match status {
            Ok(status) if status.success() => {
                let _ = job.encoder_cache.set(spec);
                info!("recording video with {}", spec.name);
                return Ok(spec);
            }
            Err(Stop::Cancelled) => return Err(Stop::Cancelled),
            _ => {}
        }
    }
    Err(VideoError::NoEncoder { tried }.into())
}

/// Receives the next frame, waking every 100 ms to check for cancel. `None` once the channel is
/// closed.
fn next_frame(job: &WriterJob, shared: &WriterShared) -> Result<Option<Frame>, Stop> {
    loop {
        match job.frames.recv_timeout(Duration::from_millis(100)) {
            Ok(frame) => return Ok(Some(frame)),
            Err(RecvTimeoutError::Disconnected) => return Ok(None),
            Err(RecvTimeoutError::Timeout) if shared.cancelled() => return Err(Stop::Cancelled),
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

/// The error for a recording that ended before any frame was encoded.
fn no_frames(job: &WriterJob, shared: &WriterShared) -> Stop {
    match shared.end.get() {
        Some(End {
            reason: VideoEnd::Stalled,
            ..
        }) => VideoError::TimedOut(job.stall_timeout).into(),
        Some(end) => VideoError::NoFrames(end.reason).into(),
        // Closed without an end: the main side dropped it.
        None => Stop::Cancelled,
    }
}

fn write(
    job: &WriterJob,
    shared: &WriterShared,
    session: &mut Session,
) -> Result<SavedVideo, Stop> {
    let spec = pick_encoder(job, session);
    let _ = shared.ready.set(
        spec.as_ref()
            .map(|spec| spec.name)
            .map_err(|stop| match stop {
                Stop::Failed(error) => error.clone(),
                Stop::Cancelled => VideoError::NoEncoder { tried: Vec::new() },
            }),
    );
    let spec = spec?;

    let Some(first) = next_frame(job, shared)? else {
        return Err(no_frames(job, shared));
    };
    let format = first.image.texture_descriptor.format;
    if !matches!(
        format,
        TextureFormat::Rgba8UnormSrgb | TextureFormat::Bgra8UnormSrgb
    ) {
        return Err(VideoError::UnsupportedFormat(format).into());
    }
    let rect = encoded_rect(first.image.size(), first.crop)?;
    let size = rect.size();

    let write_error = |err: std::io::Error| VideoError::Write {
        path: job.path.clone(),
        message: err.to_string(),
    };
    let directory = match job.path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(directory).map_err(write_error)?;
    let mut builder = tempfile::Builder::new();
    builder
        .prefix(".ename-video-")
        .suffix(job.container.suffix());
    // `tempfile` makes owner-only files and the rename keeps that.
    #[cfg(unix)]
    builder.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o666));
    let temp = builder.tempfile_in(directory).map_err(write_error)?;

    let mut child = Command::new(&job.ffmpeg)
        .args(encode_args(
            spec,
            format,
            size,
            job.frame_rate,
            job.container,
            temp.path(),
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(spawn_error)?;
    session.stdin = child.stdin.take();
    let stderr = child.stderr.take();
    // Registered before anything else can fail, so every error path reaps it.
    session.register(child)?;
    if let Some(stderr) = stderr {
        session.stderr = Some(
            std::thread::Builder::new()
                .name("video-ffmpeg-stderr".into())
                .spawn(move || drain_tail(stderr))
                .map_err(|err| ffmpeg_error(err.to_string()))?,
        );
    }

    let mut written: u64 = 0;
    let mut buffer = Vec::with_capacity((size.x * size.y * 4) as usize);
    let check = |frame: &Frame| -> Result<URect, VideoError> {
        let rect = encoded_rect(frame.image.size(), frame.crop)?;
        if rect.size() != size || frame.image.texture_descriptor.format != format {
            return Err(VideoError::Convert(format!(
                "a frame changed size or format mid-recording: {} {:?}",
                rect.size(),
                frame.image.texture_descriptor.format
            )));
        }
        Ok(rect)
    };

    match job.length {
        VideoLength::Frames(limit) => {
            let limit = u64::from(limit.get());
            let mut frame = Some(first);
            while let Some(current) = frame {
                if written < limit {
                    copy_rect(&current.image, check(&current)?, &mut buffer);
                    session.write(&buffer)?;
                    written += 1;
                }
                frame = next_frame(job, shared)?;
            }
        }
        VideoLength::Time(length) => {
            let fps = job.frame_rate;
            let total = ((length.as_secs_f64() * f64::from(fps)).round() as u64).max(1);
            // The previous frame fills every tick before the next frame's stamp.
            copy_rect(&first.image, rect, &mut buffer);
            while let Some(frame) = next_frame(job, shared)? {
                let rect = check(&frame)?;
                for _ in ticks_until(frame.stamp, fps, written, total) {
                    session.write(&buffer)?;
                    written += 1;
                }
                copy_rect(&frame.image, rect, &mut buffer);
            }
            let limit = match shared.end.get() {
                Some(End {
                    cutoff: Some(cutoff),
                    ..
                }) => ticks_until(*cutoff, fps, written, total).end,
                Some(_) => total,
                None => return Err(Stop::Cancelled),
            };
            while written < limit.max(1) {
                session.write(&buffer)?;
                written += 1;
            }
        }
    }

    let Some(end) = shared.end.get().copied() else {
        return Err(Stop::Cancelled);
    };
    // Closing stdin tells ffmpeg to finish the file.
    session.stdin = None;
    let status = session.wait(None)?;
    if !status.success() {
        return Err(ffmpeg_error(format!("ffmpeg exited with {status}")).into());
    }
    temp.as_file().sync_all().map_err(write_error)?;
    // An empty file must not replace an earlier recording at `path`.
    if temp.as_file().metadata().map_err(write_error)?.len() == 0 {
        return Err(ffmpeg_error("ffmpeg exited successfully but wrote nothing".into()).into());
    }
    // After this, cancel loses: the file is published even if the request is gone.
    if shared
        .state
        .compare_exchange(RUNNING, PUBLISHING, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(Stop::Cancelled);
    }
    temp.persist(&job.path)
        .map_err(|err| write_error(err.error))?;

    let frames = written as u32;
    Ok(SavedVideo {
        path: job.path.clone(),
        size,
        frames,
        frame_rate: job.frame_rate,
        duration: Duration::from_secs_f64(f64::from(frames) / f64::from(job.frame_rate)),
        dropped_frames: 0,
        encoder: spec.name.to_owned(),
        ended: end.reason,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use bevy::{
        asset::RenderAssetUsages,
        render::render_resource::{Extent3d, TextureDimension},
    };
    use std::sync::mpsc::{SyncSender, sync_channel};

    /// Process tests fork; one at a time, so a script being written is never inherited open by a
    /// concurrent fork (which makes exec fail with "text file busy").
    pub(crate) static PROCESS_TESTS: Mutex<()> = Mutex::new(());

    pub(crate) fn lock_processes() -> std::sync::MutexGuard<'static, ()> {
        PROCESS_TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A fake `ffmpeg`. `probe` is the shell body run for an encoder probe, `encode` the one run
    /// for a recording, with `$out` the output path. The recording writes its pid to `pid`; a
    /// probe may, with `echo $$ > "$pidfile"`.
    pub(crate) fn fake_ffmpeg(dir: &Path, probe: &str, encode: &str) -> OsString {
        let path = dir.join("fake-ffmpeg");
        let pid = dir.join("pid");
        let script = format!(
            "#!/bin/sh\n\
             pidfile='{}'\n\
             for out; do :; done\n\
             case \" $* \" in *\" -f null - \"*) {probe};; esac\n\
             echo $$ > \"$pidfile\"\n\
             {encode}\n",
            pid.display()
        );
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        path.into()
    }

    /// Whether the process whose pid the fake wrote is gone (or never started).
    pub(crate) fn fake_exited(dir: &Path) -> bool {
        match std::fs::read_to_string(dir.join("pid")) {
            Ok(pid) => process_gone(pid.trim()),
            Err(_) => true,
        }
    }

    /// `kill -0` fails once a process is reaped; a zombie still answers it.
    pub(crate) fn process_gone(pid: &str) -> bool {
        !Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    pub(crate) fn test_image(width: u32, height: u32, format: TextureFormat) -> Image {
        let mut data = Vec::new();
        for y in 0..height {
            for x in 0..width {
                data.extend([x as u8, y as u8, 200, 255]);
            }
        }
        Image::new(
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            data,
            format,
            RenderAssetUsages::default(),
        )
    }

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn ticks_fill_up_to_the_next_stamp() {
        // 30 fps: a frame at 100 ms covers ticks 0..3 (0, 33, 67 ms); one at exactly 100 ms
        // owns tick 3.
        assert_eq!(ticks_until(ms(100), 30, 0, 30), 0..3);
        assert_eq!(ticks_until(ms(101), 30, 0, 30), 0..3);
        assert_eq!(ticks_until(ms(118), 30, 0, 30), 0..4);
        // Two frames inside one tick: the second adds nothing, so the first is dropped.
        assert_eq!(ticks_until(ms(10), 30, 1, 30), 1..1);
        // A gap repeats the previous frame.
        assert_eq!(ticks_until(ms(500), 30, 4, 30), 4..15);
        // Never past the limit.
        assert_eq!(ticks_until(ms(5000), 30, 20, 30), 20..30);
    }

    /// A game at the output rate, stamps jittering 0.3 ms either way: one tick per frame, none
    /// dropped or doubled.
    #[test]
    fn jitter_around_the_ticks_gives_one_tick_per_frame() {
        let mut next = 0;
        for k in 1..600u64 {
            let jitter = if k % 2 == 0 { 0.3 } else { -0.3 };
            let stamp = Duration::from_secs_f64((k as f64 * 1000.0 / 60.0 + jitter) / 1000.0);
            let ticks = ticks_until(stamp, 60, next, 10_000);
            assert_eq!(ticks, next..k, "frame {k}");
            next = ticks.end;
        }
    }

    #[test]
    fn an_early_cutoff_bounds_the_ticks() {
        let total = 3600 * 60;
        let cutoff = Duration::from_secs(2) + Duration::from_secs(1) / 60;
        assert_eq!(ticks_until(cutoff, 60, 100, total), 100..121);
    }

    #[test]
    fn encoded_rect_rounds_to_even_and_rejects_what_cannot_be_encoded() {
        let size = UVec2::new(10, 8);
        assert_eq!(
            encoded_rect(size, None),
            Ok(URect::from_corners(UVec2::ZERO, size))
        );
        assert_eq!(
            encoded_rect(size, Some(URect::new(1, 1, 6, 4))),
            Ok(URect::new(1, 1, 5, 3))
        );
        // Clamped to the image first.
        assert_eq!(
            encoded_rect(size, Some(URect::new(6, 4, 20, 20))),
            Ok(URect::new(6, 4, 10, 8))
        );
        assert_eq!(
            encoded_rect(size, Some(URect::new(3, 3, 4, 7))),
            Err(VideoError::TooSmall {
                size: UVec2::new(1, 4)
            })
        );
        let outside = URect::new(20, 20, 30, 30);
        assert_eq!(
            encoded_rect(size, Some(outside)),
            Err(VideoError::EmptyCrop {
                crop: outside,
                size
            })
        );
    }

    #[test]
    fn copy_rect_packs_the_crop() {
        let image = test_image(4, 3, TextureFormat::Rgba8UnormSrgb);
        let mut out = Vec::new();
        copy_rect(&image, URect::new(1, 1, 3, 3), &mut out);
        assert_eq!(
            out,
            [
                1, 1, 200, 255, 2, 1, 200, 255, 1, 2, 200, 255, 2, 2, 200, 255
            ]
        );
    }

    #[test]
    fn encode_args_tag_the_colour_and_pick_the_input_format() {
        let args = |name: &str, format| -> Vec<String> {
            encode_args(
                spec(name).unwrap(),
                format,
                UVec2::new(640, 360),
                30,
                Container::Matroska,
                Path::new("/tmp/x.mkv"),
            )
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect()
        };
        let vaapi = args("h264_vaapi", TextureFormat::Bgra8UnormSrgb).join(" ");
        assert!(vaapi.contains("-pix_fmt bgra -s 640x360 -framerate 30 -i pipe:0"));
        assert!(vaapi.contains("-init_hw_device vaapi=va -filter_hw_device va"));
        assert!(vaapi.contains(
            "format=nv12,setparams=color_primaries=bt709:color_trc=iec61966-2-1:\
             colorspace=bt709:range=tv,hwupload"
        ));
        assert!(
            vaapi.ends_with("-c:v h264_vaapi -rc_mode CQP -qp 20 -g 60 -an -f matroska /tmp/x.mkv")
        );
        let x264 = args("libx264", TextureFormat::Rgba8UnormSrgb).join(" ");
        assert!(x264.contains("-pix_fmt rgba"));
        assert!(x264.contains("format=yuv420p,setparams="));
        assert!(!x264.contains("hwupload"));
    }

    #[test]
    fn containers_come_from_the_extension() {
        assert_eq!(
            Container::from_path(Path::new("a.MP4")),
            Some(Container::Mp4)
        );
        assert_eq!(
            Container::from_path(Path::new("a.mkv")),
            Some(Container::Matroska)
        );
        assert_eq!(Container::from_path(Path::new("a.webm")), None);
        assert_eq!(Container::from_path(Path::new("mp4")), None);
    }

    /// A job writing to `dir/out.mp4` through `ffmpeg`.
    fn job(dir: &Path, ffmpeg: OsString, length: VideoLength) -> (WriterJob, SyncSender<Frame>) {
        let (sender, frames) = sync_channel(8);
        let job = WriterJob {
            path: dir.join("out.mp4"),
            container: Container::Mp4,
            length,
            frame_rate: 30,
            frames,
            ffmpeg,
            forced_encoder: Some("libx264".into()),
            probe_timeout: Duration::from_secs(5),
            stall_timeout: Duration::from_secs(5),
            encoder_cache: default(),
        };
        (job, sender)
    }

    fn frame(size: u32, stamp: Duration) -> Frame {
        Frame {
            image: Arc::new(test_image(size, size, TextureFormat::Rgba8UnormSrgb)),
            crop: None,
            stamp,
        }
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(ms(10));
        }
    }

    fn only_entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    fn frames(n: u32) -> VideoLength {
        VideoLength::Frames(std::num::NonZeroU32::new(n).unwrap())
    }

    #[cfg(unix)]
    #[test]
    fn a_child_registered_after_cancel_is_killed() {
        let _guard = lock_processes();
        let shared = WriterShared::new();
        shared.cancel();
        let mut session = Session {
            shared: &shared,
            stdin: None,
            stderr: None,
        };
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        assert!(matches!(session.register(child), Err(Stop::Cancelled)));
        session.shutdown();
        assert!(process_gone(&pid.to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn a_good_recording_is_published() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let ffmpeg = fake_ffmpeg(dir.path(), "exit 0", "exec cat > \"$out\"");
        let (job, sender) = job(dir.path(), ffmpeg, frames(2));
        let (shared, thread) = spawn_writer(job);
        for _ in 0..2 {
            sender.send(frame(4, Duration::ZERO)).unwrap();
        }
        shared.end(End {
            reason: VideoEnd::Completed,
            cutoff: None,
        });
        drop(sender);
        thread.join().unwrap();
        let saved = shared.result.get().unwrap().clone().unwrap();
        assert_eq!((saved.frames, saved.size), (2, UVec2::new(4, 4)));
        assert_eq!(std::fs::metadata(&saved.path).unwrap().len(), 2 * 4 * 4 * 4);
        assert_eq!(only_entries(dir.path()), ["fake-ffmpeg", "out.mp4", "pid"]);
        // After publishing, cancel loses.
        shared.cancel();
        assert!(!shared.cancelled());
    }

    #[cfg(unix)]
    #[test]
    fn an_empty_output_does_not_replace_an_earlier_file() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let ffmpeg = fake_ffmpeg(dir.path(), "exit 0", "cat > /dev/null; exit 0");
        let (job, sender) = job(dir.path(), ffmpeg, frames(1));
        std::fs::write(&job.path, b"earlier").unwrap();
        let path = job.path.clone();
        let (shared, thread) = spawn_writer(job);
        sender.send(frame(4, Duration::ZERO)).unwrap();
        shared.end(End {
            reason: VideoEnd::Completed,
            cutoff: None,
        });
        drop(sender);
        thread.join().unwrap();
        assert!(
            matches!(shared.result.get(), Some(Err(VideoError::Ffmpeg { status, .. })) if status.contains("wrote nothing")),
            "{:?}",
            shared.result.get()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"earlier");
        assert_eq!(only_entries(dir.path()), ["fake-ffmpeg", "out.mp4", "pid"]);
    }

    #[cfg(unix)]
    #[test]
    fn cancel_wakes_a_blocked_write_and_reaps_the_child() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let ffmpeg = fake_ffmpeg(dir.path(), "exit 0", "exec sleep 30");
        let (job, sender) = job(dir.path(), ffmpeg, frames(100));
        let (shared, thread) = spawn_writer(job);
        // 64 KiB a frame: the pipe fills and a write blocks.
        for _ in 0..4 {
            sender.send(frame(128, Duration::ZERO)).unwrap();
        }
        wait_for("a blocked write", || shared.stuck_for() > ms(100));
        let started = Instant::now();
        shared.cancel();
        wait_for("the writer", || thread.is_finished());
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(fake_exited(dir.path()));
        assert!(shared.result.get().is_none());
        assert_eq!(only_entries(dir.path()), ["fake-ffmpeg", "pid"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_noisy_failing_ffmpeg_reports_its_stderr_tail() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let ffmpeg = fake_ffmpeg(
            dir.path(),
            "exit 0",
            "head -c 2000000 /dev/zero | tr '\\0' x >&2; echo the-end >&2; exit 1",
        );
        let (job, sender) = job(dir.path(), ffmpeg, frames(1));
        let (shared, thread) = spawn_writer(job);
        sender.send(frame(4, Duration::ZERO)).unwrap();
        shared.end(End {
            reason: VideoEnd::Completed,
            cutoff: None,
        });
        drop(sender);
        thread.join().unwrap();
        let Some(Err(VideoError::Ffmpeg { stderr, .. })) = shared.result.get() else {
            panic!("{:?}", shared.result.get());
        };
        assert!(
            stderr.len() <= 4096 && stderr.ends_with("the-end"),
            "{}",
            stderr.len()
        );
        assert_eq!(only_entries(dir.path()), ["fake-ffmpeg", "pid"]);
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_wakes_an_idle_writer_and_a_slow_probe() {
        let _guard = lock_processes();
        let dir = tempfile::tempdir().unwrap();
        let idle = fake_ffmpeg(dir.path(), "exit 0", "exec cat > \"$out\"");
        let (idle, idle_sender) = job(dir.path(), idle, frames(1));
        let idle = spawn_writer(idle);
        wait_for("the probe", || idle.0.ready.get().is_some());

        let probing_dir = tempfile::tempdir().unwrap();
        let probing = fake_ffmpeg(
            probing_dir.path(),
            "echo $$ > \"$pidfile\"; exec sleep 30",
            "",
        );
        let (probing, _probing_sender) = job(probing_dir.path(), probing, frames(1));
        let probing = spawn_writer(probing);
        wait_for("the probe to start", || {
            probing_dir.path().join("pid").exists()
        });

        let started = Instant::now();
        shutdown_writers(vec![idle, probing], Duration::from_secs(2));
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(fake_exited(probing_dir.path()));
        drop(idle_sender);
    }

    /// Needs a real ffmpeg with libx264; skipped without one.
    #[test]
    fn real_ffmpeg_writes_a_tagged_h264_file() {
        let _guard = lock_processes();
        if Command::new("ffprobe").arg("-version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (job, sender) = job(dir.path(), "ffmpeg".into(), VideoLength::Time(ms(1000)));
        let (shared, thread) = spawn_writer(job);
        for i in 0..10 {
            sender.send(frame(64, ms(i * 100))).unwrap();
        }
        shared.end(End {
            reason: VideoEnd::Completed,
            cutoff: None,
        });
        drop(sender);
        thread.join().unwrap();
        let saved = shared.result.get().unwrap().clone().unwrap();
        assert_eq!((saved.frames, saved.encoder.as_str()), (30, "libx264"));
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-count_frames",
                "-show_entries",
                "stream=codec_name,width,nb_read_frames,color_transfer,color_primaries",
                "-of",
                "csv=p=0",
            ])
            .arg(&saved.path)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&probe.stdout).trim(),
            "h264,64,iec61966-2-1,bt709,30"
        );
    }
}
