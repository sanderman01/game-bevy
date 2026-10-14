//! `video_record`, `video_status` and `video_stop`.
//!
//! A recording can outlast any sane tool call, so `video_record` waits at most [`MAX_BLOCK`] and
//! then returns the request id with the recording still running; `video_status` collects it.
//! Every tool runs under one hard deadline covering every request it makes, the run id included,
//! because the HTTP client has no timeout of its own. The soft cap is the poll loop choosing to
//! return; the hard deadline only catches a game that stopped answering.

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

use rmcp::{ErrorData, handler::server::wrapper::Parameters, schemars, tool, tool_router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    CaptureSource, GameServer, ToolResult, explain_inactive_camera, explain_missing_editor_method,
    fail,
};
use crate::brp::{BrpError, METHOD_NOT_FOUND};

/// The longest a tool waits for a recording before returning it still running. Under the 60 s
/// request timeout many MCP clients inherit.
const MAX_BLOCK: Duration = Duration::from_secs(50);
/// Room on top of a soft cap for the last status call and the run id.
const HARD_MARGIN: Duration = Duration::from_secs(10);
/// The engine's finalize deadline. `video_stop` waits this long for the file.
const FINALIZE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct VideoRecordParams {
    /// Where to write the video: `.mp4` or `.mkv`, H.264. A relative path resolves against this
    /// MCP server's working directory, normally the project root.
    pub path: String,
    /// "window" is the whole game window. "game_camera" is the stage's MainCamera (in the
    /// editor, the Game View). "editor_camera" is the editor's Scene View. A camera source needs
    /// its view shown for the whole recording; see editor_set_view.
    pub source: CaptureSource,
    /// How long to record. Exactly one key: {"frames": 600} records 600 rendered frames;
    /// {"seconds": 10} records 10 s of real time; {"timecode": "00:01:30"} is hh:mm:ss. At most
    /// 24 hours.
    pub duration: VideoDuration,
    /// Output frames per second, 1 to 240. Defaults to 60.
    #[serde(default)]
    pub frame_rate: Option<u32>,
    /// Wait for the file, up to 50 seconds. Defaults to true. A longer recording comes back
    /// still running, with a request id for video_status.
    #[serde(default)]
    pub wait: Option<bool>,
}

/// A length with its unit as the key, so `10` is never read as frames when seconds were meant.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum VideoDuration {
    Frames(u32),
    Seconds(f64),
    Timecode(String),
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct VideoStatusParams {
    /// The request id video_record returned.
    pub request: u64,
    /// Wait up to this many seconds (at most 50) for the recording to finish. Defaults to 0.
    #[serde(default)]
    pub wait_seconds: Option<f64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct VideoStopParams {
    /// The request id video_record returned.
    pub request: u64,
}

/// A recording's state as the game reports it, plus its request id.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VideoStatus {
    /// Pass to video_status or video_stop.
    #[serde(default)]
    request: u64,
    /// "starting" (choosing an encoder), "recording", "finalizing" or "saved".
    status: String,
    /// Frames recorded so far, or in the file once saved.
    #[serde(default)]
    frames: u32,
    /// Frames lost because the encoder or the GPU readback fell behind.
    #[serde(default)]
    dropped_frames: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    elapsed_seconds: Option<f64>,
    /// Absolute. Present once saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frame_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration_seconds: Option<f64>,
    /// The ffmpeg encoder used, such as h264_vaapi.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    encoder: Option<String>,
    /// Why it ended: "completed", or why it ended early ("stopped", "source_resized",
    /// "camera_inactive", "source_lost", "stalled").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ended: Option<String>,
    /// What to do next, when there is something.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

impl VideoStatus {
    fn saved(&self) -> bool {
        self.status == "saved"
    }
}

/// How long one tool call follows a recording.
#[derive(Clone, Copy)]
struct Follow {
    /// Return once the recording has left "starting", rather than waiting for the file.
    until_started: bool,
    soft: Duration,
    hard: Duration,
}

/// What the poll loop last knew, for the message when the hard deadline fires.
#[derive(Default)]
struct Progress {
    request: Option<u64>,
    last: Option<(VideoStatus, Instant)>,
}

#[tool_router(router = video_router, vis = "pub(super)")]
impl GameServer {
    #[tool(
        description = "Record the game window, the game camera or the editor camera to an .mp4 \
                       or .mkv file. Waits for the file up to 50 seconds; a longer recording \
                       returns its request id while still running."
    )]
    async fn video_record(
        &self,
        Parameters(params): Parameters<VideoRecordParams>,
    ) -> ToolResult<VideoStatus> {
        let cwd = std::env::current_dir()
            .map_err(|err| fail(anyhow::anyhow!("cannot read the working directory: {err}")))?;
        let follow = Follow {
            until_started: params.wait == Some(false),
            soft: MAX_BLOCK,
            hard: MAX_BLOCK + HARD_MARGIN,
        };
        self.record_video(params, &cwd, follow).await
    }

    #[tool(
        description = "Read a video recording's progress, optionally waiting for it to finish. \
                       Returns the file's details once saved."
    )]
    async fn video_status(
        &self,
        Parameters(params): Parameters<VideoStatusParams>,
    ) -> ToolResult<VideoStatus> {
        let wait = Duration::from_secs_f64(params.wait_seconds.unwrap_or(0.0).clamp(0.0, 50.0));
        let follow = Follow {
            until_started: false,
            soft: wait,
            hard: wait + HARD_MARGIN,
        };
        self.follow_video(params.request, None, follow, None).await
    }

    #[tool(
        description = "Stop a video recording early and wait for its file. What was recorded is \
                       kept."
    )]
    async fn video_stop(
        &self,
        Parameters(params): Parameters<VideoStopParams>,
    ) -> ToolResult<VideoStatus> {
        let follow = Follow {
            until_started: false,
            soft: FINALIZE,
            hard: FINALIZE + HARD_MARGIN,
        };
        self.follow_video(params.request, None, follow, Some("game.video.stop"))
            .await
    }
}

impl GameServer {
    async fn record_video(
        &self,
        params: VideoRecordParams,
        cwd: &Path,
        follow: Follow,
    ) -> ToolResult<VideoStatus> {
        let path = absolute_video_path(&params.path, cwd)
            .map_err(|message| ErrorData::invalid_params(message, None))?;
        let source = params.source;
        let mut body = json!({ "path": path, "duration": params.duration });
        if let Some(frame_rate) = params.frame_rate {
            body["frame_rate"] = json!(frame_rate);
        }
        let method = match source {
            CaptureSource::Window => {
                body["source"] = json!("window");
                "game.video.record"
            }
            CaptureSource::GameCamera => {
                body["source"] = json!("main_camera");
                "game.video.record"
            }
            CaptureSource::EditorCamera => "editor.video.record",
        };
        let progress = Mutex::new(Progress::default());
        let started = Instant::now();
        let work = async {
            let response = self
                .brp
                .call_raw(method, body)
                .await
                .map_err(explain_missing_video_method)
                .map_err(explain_missing_editor_method)
                .map_err(fail)?;
            let request = response["request"].as_u64().ok_or_else(|| {
                fail(anyhow::anyhow!(
                    "`{method}` returned no request id: {response}"
                ))
            })?;
            self.poll_video(request, Some(source), follow, started, &progress)
                .await
        };
        timed(follow.hard, work, &progress).await
    }

    /// Optionally sends `first` (a stop), then follows the recording.
    async fn follow_video(
        &self,
        request: u64,
        source: Option<CaptureSource>,
        follow: Follow,
        first: Option<&str>,
    ) -> ToolResult<VideoStatus> {
        let progress = Mutex::new(Progress {
            request: Some(request),
            last: None,
        });
        let started = Instant::now();
        let work = async {
            if let Some(method) = first {
                self.brp
                    .call_raw(method, json!({ "request": request }))
                    .await
                    .map_err(explain_missing_video_method)
                    .map_err(fail)?;
            }
            self.poll_video(request, source, follow, started, &progress)
                .await
        };
        timed(follow.hard, work, &progress).await
    }

    async fn poll_video(
        &self,
        request: u64,
        source: Option<CaptureSource>,
        follow: Follow,
        started: Instant,
        progress: &Mutex<Progress>,
    ) -> ToolResult<VideoStatus> {
        progress.lock().unwrap().request = Some(request);
        let mut status = loop {
            let mut status: VideoStatus = self
                .brp
                .call("game.video.status", json!({ "request": request }))
                .await
                .map_err(explain_missing_video_method)
                .map_err(|error| match source {
                    Some(source) => fail(explain_inactive_camera(error, source)),
                    None => fail(error),
                })?;
            status.request = request;
            progress.lock().unwrap().last = Some((status.clone(), Instant::now()));
            let done = status.saved()
                || (follow.until_started && status.status != "starting")
                || started.elapsed() >= follow.soft;
            if done {
                break status;
            }
            tokio::time::sleep(POLL).await;
        };
        status.note = note(&status);
        self.tag(status).await
    }
}

/// Runs `work` under the hard deadline, wording a timeout from what the loop last knew.
async fn timed(
    deadline: Duration,
    work: impl Future<Output = ToolResult<VideoStatus>>,
    progress: &Mutex<Progress>,
) -> ToolResult<VideoStatus> {
    tokio::time::timeout(deadline, work).await.map_err(|_| {
        let progress = progress.lock().unwrap();
        let seconds = deadline.as_secs_f32();
        fail(match (&progress.request, &progress.last) {
            (None, _) => anyhow::anyhow!(
                "the game stopped answering before the recording started, within {seconds} \
                 seconds. It may be hung or stopped at a breakpoint."
            ),
            (Some(request), Some((last, at))) => anyhow::anyhow!(
                "the game stopped answering. Request {request} was last {} at {} frames, {:.1} \
                 seconds ago. Check it with video_status.",
                last.status,
                last.frames,
                at.elapsed().as_secs_f32()
            ),
            (Some(request), None) => anyhow::anyhow!(
                "the game stopped answering about request {request} within {seconds} seconds. \
                 Check it with video_status."
            ),
        })
    })?
}

fn note(status: &VideoStatus) -> Option<String> {
    match (status.status.as_str(), status.ended.as_deref()) {
        ("saved", Some("completed") | None) => None,
        ("saved", Some(ended)) => Some(format!(
            "ended early ({ended}) after {} frames; the file holds what was recorded",
            status.frames
        )),
        _ => Some(format!(
            "still {}; call video_status with request {} to wait for it, or video_stop to end \
             it",
            status.status, status.request
        )),
    }
}

/// Rewords "method not found" from a `game.video.*` call.
fn explain_missing_video_method(error: anyhow::Error) -> anyhow::Error {
    match error.downcast_ref::<BrpError>() {
        Some(brp) if brp.code == METHOD_NOT_FOUND && brp.method.starts_with("game.video.") => {
            anyhow::anyhow!(
                "this game build was compiled without the `video` feature. Run the editor binary \
                 with its default features."
            )
        }
        _ => error,
    }
}

/// Makes `path` absolute against the sidecar's working directory. Refuses anything that is not
/// `.mp4` or `.mkv`.
fn absolute_video_path(path: &str, cwd: &Path) -> Result<PathBuf, String> {
    let path = Path::new(path);
    let supported = path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("mp4") || extension.eq_ignore_ascii_case("mkv")
    });
    if !supported {
        return Err(format!(
            "`{}` must end in .mp4 or .mkv; video is only written as H.264 in those",
            path.display()
        ));
    }
    Ok(cwd.join(path))
}

#[cfg(test)]
mod tests {
    use super::super::tests::{Responder, expect_err, serve};
    use super::*;
    use crate::brp::BrpClient;
    use serde_json::Value;
    use std::sync::{Arc, atomic::AtomicUsize, atomic::Ordering};

    type Seen = Arc<Mutex<Vec<(String, Value)>>>;

    /// A game that answers video calls. `status` gives the status replies in order, repeating
    /// the last; `stall` names a method (and its nth call, from 0) never to answer.
    async fn video_game(
        statuses: Vec<Value>,
        stall: Option<(&'static str, usize)>,
    ) -> (String, Seen) {
        let seen: Seen = Arc::default();
        let log = seen.clone();
        let counts: Arc<Mutex<std::collections::HashMap<String, usize>>> = Arc::default();
        let polls = Arc::new(AtomicUsize::new(0));
        let respond: Responder = Arc::new(move |_, request: &Value| {
            let method = request["method"].as_str().unwrap().to_owned();
            log.lock()
                .unwrap()
                .push((method.clone(), request["params"].clone()));
            let nth = {
                let mut counts = counts.lock().unwrap();
                let count = counts.entry(method.clone()).or_default();
                *count += 1;
                *count - 1
            };
            if stall == Some((method.as_str(), nth)) {
                return None;
            }
            Some(match method.as_str() {
                "game.pid.get" => json!({ "result": { "pid": "abc" } }),
                "game.video.status" => {
                    let index = polls.fetch_add(1, Ordering::SeqCst).min(statuses.len() - 1);
                    statuses[index].clone()
                }
                _ => json!({ "result": { "request": 7 } }),
            })
        });
        (serve(respond).await, seen)
    }

    fn status(status: &str, frames: u32) -> Value {
        json!({ "result": { "status": status, "frames": frames, "dropped_frames": 0, "elapsed_seconds": 0.5 } })
    }

    fn saved(ended: &str) -> Value {
        json!({ "result": {
            "status": "saved", "path": "/abs/a.mp4", "width": 640, "height": 360, "frames": 600,
            "frame_rate": 60, "duration_seconds": 10.0, "dropped_frames": 0,
            "encoder": "h264_vaapi", "ended": ended,
        } })
    }

    fn params(path: &str, source: CaptureSource) -> VideoRecordParams {
        VideoRecordParams {
            path: path.into(),
            source,
            duration: VideoDuration::Seconds(10.0),
            frame_rate: None,
            wait: None,
        }
    }

    fn follow(soft: Duration, hard: Duration) -> Follow {
        Follow {
            until_started: false,
            soft,
            hard,
        }
    }

    fn short() -> Follow {
        follow(Duration::from_secs(5), Duration::from_secs(6))
    }

    fn result(json: &rmcp::Json<super::super::Tagged<VideoStatus>>) -> Value {
        serde_json::to_value(&json.0).unwrap()
    }

    #[tokio::test]
    async fn record_routes_each_source_and_waits_for_the_file() {
        for (source, method, game_source) in [
            (CaptureSource::Window, "game.video.record", Some("window")),
            (
                CaptureSource::GameCamera,
                "game.video.record",
                Some("main_camera"),
            ),
            (CaptureSource::EditorCamera, "editor.video.record", None),
        ] {
            let (url, seen) = video_game(
                vec![
                    status("starting", 0),
                    status("recording", 3),
                    saved("completed"),
                ],
                None,
            )
            .await;
            let server = GameServer::new(BrpClient::new(url));
            let mut record = params("clips/a.mp4", source);
            record.duration = VideoDuration::Frames(600);
            record.frame_rate = Some(30);
            let done = server
                .record_video(record, Path::new("/proj"), short())
                .await
                .unwrap();
            let done = result(&done);
            assert_eq!(done["status"], "saved");
            assert_eq!(done["request"], 7);
            assert!(done.get("note").is_none());
            let seen = seen.lock().unwrap();
            assert_eq!(seen[0].0, method);
            assert_eq!(seen[0].1["path"], "/proj/clips/a.mp4");
            assert_eq!(seen[0].1["duration"], json!({ "frames": 600 }));
            assert_eq!(seen[0].1["frame_rate"], 30);
            assert_eq!(seen[0].1.get("source").and_then(Value::as_str), game_source);
            assert_eq!(
                seen.iter()
                    .filter(|(m, _)| m == "game.video.status")
                    .count(),
                3
            );
        }
    }

    #[tokio::test]
    async fn record_refuses_other_formats_without_calling() {
        let (url, seen) = video_game(vec![saved("completed")], None).await;
        let server = GameServer::new(BrpClient::new(url));
        let err = expect_err(
            server
                .record_video(
                    params("a.png", CaptureSource::Window),
                    Path::new("/p"),
                    short(),
                )
                .await,
        );
        assert!(err.message.contains(".mp4 or .mkv"), "{}", err.message);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_soft_cap_returns_a_running_recording() {
        let (url, _) = video_game(vec![status("recording", 12)], None).await;
        let server = GameServer::new(BrpClient::new(url));
        let running = server
            .record_video(
                params("/a.mp4", CaptureSource::Window),
                Path::new("/"),
                follow(Duration::from_millis(250), Duration::from_secs(5)),
            )
            .await
            .unwrap();
        let running = result(&running);
        assert_eq!(running["status"], "recording");
        assert_eq!(running["request"], 7);
        assert!(running["note"].as_str().unwrap().contains("video_status"));
    }

    #[tokio::test]
    async fn without_waiting_record_returns_once_started() {
        let (url, seen) =
            video_game(vec![status("starting", 0), status("recording", 1)], None).await;
        let server = GameServer::new(BrpClient::new(url));
        let started = server
            .record_video(
                params("/a.mp4", CaptureSource::Window),
                Path::new("/"),
                Follow {
                    until_started: true,
                    ..short()
                },
            )
            .await
            .unwrap();
        assert_eq!(result(&started)["status"], "recording");
        let polls = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| method == "game.video.status")
            .count();
        assert_eq!(polls, 2);
    }

    #[tokio::test]
    async fn a_game_that_stops_answering_hits_the_hard_deadline() {
        let deadline = follow(Duration::from_secs(5), Duration::from_millis(400));
        for (stall, expected) in [
            (("game.video.record", 0), "before the recording started"),
            (("game.video.status", 2), "was last recording at 3 frames"),
            (("game.pid.get", 0), "was last saved"),
        ] {
            let (url, _) = video_game(
                vec![
                    status("recording", 3),
                    status("recording", 3),
                    saved("completed"),
                ],
                Some(stall),
            )
            .await;
            let server = GameServer::new(BrpClient::new(url));
            let err = expect_err(
                server
                    .record_video(
                        params("/a.mp4", CaptureSource::Window),
                        Path::new("/"),
                        deadline,
                    )
                    .await,
            );
            assert!(err.message.contains(expected), "{stall:?}: {}", err.message);
        }
    }

    #[tokio::test]
    async fn status_waits_and_stop_polls_to_the_file() {
        let (url, _) = video_game(vec![status("recording", 3), saved("completed")], None).await;
        let server = GameServer::new(BrpClient::new(url));
        let once = server.follow_video(
            7,
            None,
            follow(Duration::ZERO, Duration::from_secs(5)),
            None,
        );
        assert_eq!(result(&once.await.unwrap())["status"], "recording");
        let waited = server.follow_video(7, None, short(), None).await.unwrap();
        assert_eq!(result(&waited)["status"], "saved");

        let (url, seen) = video_game(vec![status("finalizing", 3), saved("stopped")], None).await;
        let server = GameServer::new(BrpClient::new(url));
        let stopped = server
            .follow_video(7, None, short(), Some("game.video.stop"))
            .await
            .unwrap();
        let stopped = result(&stopped);
        assert_eq!(stopped["ended"], "stopped");
        assert!(stopped["note"].as_str().unwrap().contains("ended early"));
        assert_eq!(
            seen.lock().unwrap()[0],
            ("game.video.stop".into(), json!({ "request": 7 }))
        );
    }

    #[tokio::test]
    async fn an_inactive_camera_gets_the_view_hint() {
        let error = json!({ "error": {
            "code": -32603, "message": "camera 4v0 is not active", "data": { "kind": "camera_inactive" },
        } });
        let (url, _) = video_game(vec![error], None).await;
        let server = GameServer::new(BrpClient::new(url));
        let err = expect_err(
            server
                .record_video(
                    params("/a.mp4", CaptureSource::GameCamera),
                    Path::new("/"),
                    short(),
                )
                .await,
        );
        assert!(
            err.message.contains("editor_set_view with view=game"),
            "{}",
            err.message
        );
    }

    #[tokio::test]
    async fn a_build_without_video_says_so() {
        let missing =
            json!({ "error": { "code": METHOD_NOT_FOUND, "message": "Method not found" } });
        let respond: Responder = Arc::new(move |_, _: &Value| Some(missing.clone()));
        let server = GameServer::new(BrpClient::new(serve(respond).await));
        let err = expect_err(
            server
                .record_video(
                    params("/a.mp4", CaptureSource::Window),
                    Path::new("/"),
                    short(),
                )
                .await,
        );
        assert!(
            err.message.contains("without the `video` feature"),
            "{}",
            err.message
        );
        let err = expect_err(
            server
                .record_video(
                    params("/a.mp4", CaptureSource::EditorCamera),
                    Path::new("/"),
                    short(),
                )
                .await,
        );
        assert!(
            err.message.contains("editor camera video"),
            "{}",
            err.message
        );
    }
}
