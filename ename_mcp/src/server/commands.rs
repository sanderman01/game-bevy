//! The game's commands and jobs: `command_list`, `command_call`, `job_list`, `job_wait`,
//! `job_cancel`, and the generic call every per-command tool goes through.
//!
//! None of this knows any command. A command answers at once or starts a job; a job is followed
//! for up to [`MAX_BLOCK`], then returned still running with its id for `job_wait`. A whole call
//! runs under one hard deadline, on top of each request's own timeout: the soft cap is the poll
//! loop choosing to return, the hard deadline only catches a game that stopped answering, and its
//! message says how the game died when the run record does. Failures come back as tool results with `isError`, because a client may show only
//! the message of a JSON-RPC error and drop its data.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use rmcp::{
    ErrorData,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, JsonObject},
    schemars, tool, tool_router,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::GameServer;
use crate::{
    brp::{BrpError, METHOD_NOT_FOUND},
    notice::{LogNotice, Tally},
    staleness,
};

/// The longest a tool waits for a job before returning it still running. Under the 60 s request
/// timeout many MCP clients inherit.
pub const MAX_BLOCK: Duration = Duration::from_secs(50);
/// Room on top of the soft cap for the last status call: one request's timeout.
pub const HARD_MARGIN: Duration = crate::brp::REQUEST_TIMEOUT;
const POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct CommandListParams {
    /// Case-insensitive substring of the command name.
    #[serde(default)]
    pub contains: Option<String>,
    /// Include each command's params schema. Off by default: schemas are large.
    #[serde(default)]
    pub with_schemas: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CommandCallParams {
    /// The dotted command name, such as "screenshot.capture". command_list lists them.
    pub command: String,
    /// The command's params object, as its schema in command_list describes.
    #[serde(default)]
    pub params: Option<JsonObject>,
    /// For a job command: how long to wait for the result, 0 to 50 seconds. Defaults to 50.
    #[serde(default)]
    pub wait_seconds: Option<f64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct JobParams {
    /// The job id a command returned, such as "kqvzmbta-3".
    pub job: String,
    /// How long to wait for the job to finish, 0 to 50 seconds. Defaults to 50 for job_cancel and
    /// job_wait; 0 reads the status at once.
    #[serde(default)]
    pub wait_seconds: Option<f64>,
}

/// A job as the game reports it. Only what the follow loop reads is typed.
#[derive(Clone, Debug, Deserialize)]
struct JobStatus {
    id: String,
    state: String,
    #[serde(default)]
    elapsed_seconds: f64,
    #[serde(default)]
    progress: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<GameError>,
}

/// A command error, from a failed job or a BRP error's `data`.
#[derive(Clone, Debug, Default, Deserialize)]
struct GameError {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    message: String,
    #[serde(default)]
    hint: Option<String>,
    /// The run and log tally a BRP error's `data` carries.
    #[serde(default)]
    run: Option<String>,
    #[serde(default)]
    log: Option<Tally>,
}

#[derive(Deserialize)]
struct JobAnswer {
    run: String,
    job: JobStatus,
    #[serde(default)]
    log: Option<Tally>,
}

/// What the follow loop last saw, for the message when the hard deadline fires.
#[derive(Default)]
struct Seen {
    job: Option<(JobStatus, Instant)>,
}

#[tool_router(router = commands_router, vis = "pub(super)")]
impl GameServer {
    #[tool(
        description = "List the commands the running game registers, the functionality its \
                       plugins expose: screenshots, video, editor views and more. Each also \
                       appears as its own tool while the game runs; command_call reaches any of \
                       them by name."
    )]
    async fn command_list(
        &self,
        Parameters(params): Parameters<CommandListParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let answer = match self.read("game.command.list").await {
            Ok(answer) => answer,
            Err(failure) => return Ok(failure),
        };
        let run = answer["run"].as_str().unwrap_or_default().to_owned();
        let needle = params.contains.map(|text| text.to_lowercase());
        let commands: Vec<Value> = answer["commands"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|command| {
                needle.as_ref().is_none_or(|needle| {
                    command["name"]
                        .as_str()
                        .is_some_and(|name| name.to_lowercase().contains(needle))
                })
            })
            .map(|command| {
                let name = command["name"].as_str().unwrap_or_default();
                let tool = if self.catalog.is_shadowed(name) {
                    "command_call".to_owned()
                } else {
                    super::catalog::tool_name(name)
                };
                let mut entry = json!({
                    "name": name,
                    "tool": tool,
                    "description": command["description"],
                    "kind": command["kind"],
                    "scope": command["scope"],
                    "read_only": command["read_only"],
                });
                if params.with_schemas {
                    entry["params"] = command["params"].clone();
                }
                entry
            })
            .collect();
        Ok(self.success(json!({ "commands": commands }), &run, &answer))
    }

    #[tool(
        description = "Run a game command by its dotted name with a params object. For a job \
                       command, waits up to wait_seconds (default and maximum 50) and then returns \
                       a job id for job_wait."
    )]
    async fn command_call(
        &self,
        Parameters(params): Parameters<CommandCallParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut arguments = params.params.unwrap_or_default();
        if let Some(wait) = params.wait_seconds {
            arguments.insert("wait_seconds".into(), json!(wait));
        }
        // No `run`: the agent named the command itself, rather than through a catalogue that a
        // restart may have outdated.
        Ok(self.call_command(&params.command, arguments, None).await)
    }

    #[tool(description = "List the game's running jobs and the finished ones it still keeps.")]
    async fn job_list(&self) -> Result<CallToolResult, ErrorData> {
        match self.read("game.job.list").await {
            Ok(answer) => {
                let run = answer["run"].as_str().unwrap_or_default().to_owned();
                Ok(self.success(json!({ "jobs": answer["jobs"] }), &run, &answer))
            }
            Err(failure) => Ok(failure),
        }
    }

    #[tool(
        description = "Wait for a job to finish, up to wait_seconds (default and maximum 50), and \
                       return its result. wait_seconds 0 reads its status at once."
    )]
    async fn job_wait(
        &self,
        Parameters(params): Parameters<JobParams>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(self.follow_job("game.job.status", &params).await)
    }

    #[tool(
        description = "End a job early and wait for its final state, up to wait_seconds (default \
                       and maximum 50). What cancelling keeps depends on the job: a recording \
                       stops and keeps its file."
    )]
    async fn job_cancel(
        &self,
        Parameters(params): Parameters<JobParams>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(self.follow_job("game.job.cancel", &params).await)
    }
}

impl GameServer {
    /// Runs `command` with `arguments`, following a job it starts. `run` is the run the caller's
    /// catalogue came from; the game refuses the call if it restarted since.
    pub(super) async fn call_command(
        &self,
        command: &str,
        mut arguments: JsonObject,
        run: Option<String>,
    ) -> CallToolResult {
        // The sidecar's own parameter: params structs refuse fields they do not know.
        let wait = wait_for(arguments.remove("wait_seconds").as_ref(), MAX_BLOCK);
        let mut body = json!({ "command": command, "params": arguments });
        if let Some(cwd) = &self.cwd {
            body["cwd"] = json!(cwd);
        }
        if let Some(run) = &run {
            body["run"] = json!(run);
        }
        let seen = Mutex::new(Seen::default());
        let work = async {
            let answer = match self.brp.call_raw("game.command.call", body).await {
                Ok(answer) => answer,
                Err(err) => return self.brp_failure(err).await,
            };
            let run = answer["run"].as_str().unwrap_or_default().to_owned();
            if let Some(result) = answer.get("result") {
                return self.success(result.clone(), &run, &answer);
            }
            let tally = tally_of(&answer);
            match serde_json::from_value::<JobStatus>(answer["job"].clone()) {
                Ok(status) => self.follow(status, run, tally, wait, &seen).await,
                Err(err) => failure(format!(
                    "`game.command.call` answered with neither a result nor a job: {err}"
                )),
            }
        };
        self.under_deadline(wait, work, &seen).await
    }

    /// One parameterless read under the hard deadline.
    async fn read(&self, method: &str) -> Result<Value, CallToolResult> {
        match tokio::time::timeout(self.hard_margin, self.brp.call_raw(method, json!({}))).await {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(err)) => Err(self.brp_failure(err).await),
            Err(_) => Err(failure(self.with_cause(format!(
                "the game stopped answering: `{method}` had no answer within {} seconds. It may \
                 be hung or stopped at a breakpoint.",
                self.hard_margin.as_secs_f32()
            )))),
        }
    }

    /// `text`, then how the game died when its run record says.
    fn with_cause(&self, text: String) -> String {
        match self.brp.unreachable() {
            Some(cause) => format!("{text} {cause}"),
            None => text,
        }
    }

    /// `job_wait` and `job_cancel`: one call to `method`, then the follow loop.
    async fn follow_job(&self, method: &str, params: &JobParams) -> CallToolResult {
        let wait = wait_for(params.wait_seconds.map(Value::from).as_ref(), MAX_BLOCK);
        let seen = Mutex::new(Seen::default());
        let work = async {
            let answer = match self
                .brp
                .call_raw(method, json!({ "job": params.job }))
                .await
            {
                Ok(answer) => answer,
                Err(err) => return self.brp_failure(err).await,
            };
            match serde_json::from_value::<JobAnswer>(answer) {
                Ok(JobAnswer { run, job, log }) => self.follow(job, run, log, wait, &seen).await,
                Err(err) => failure(format!("`{method}` answered in an unexpected shape: {err}")),
            }
        };
        self.under_deadline(wait, work, &seen).await
    }

    /// Polls a job until it finishes or `wait` passes. `tally` is the log tally of the answer
    /// that carried `status`; only the one on the final result becomes a notice.
    async fn follow(
        &self,
        mut status: JobStatus,
        run: String,
        mut tally: Option<Tally>,
        wait: Duration,
        seen: &Mutex<Seen>,
    ) -> CallToolResult {
        let started = Instant::now();
        loop {
            seen.lock().unwrap().job = Some((status.clone(), Instant::now()));
            match status.state.as_str() {
                "succeeded" => {
                    let log = self.notice(&run, tally.as_ref());
                    return success(status.result.unwrap_or(Value::Null), &run, log);
                }
                "failed" => {
                    let error = status.error.unwrap_or_default();
                    let log = self.notice(&run, tally.as_ref());
                    return failure(with_notice(self.describe(&error), log));
                }
                "cancelled" => {
                    return failure(format!(
                        "job `{}` was cancelled before it produced a result",
                        status.id
                    ));
                }
                _ if started.elapsed() >= wait => {
                    return running(&status, &run, self.notice(&run, tally.as_ref()));
                }
                _ => {}
            }
            tokio::time::sleep(POLL).await;
            let answer = match self
                .brp
                .call_raw("game.job.status", json!({ "job": status.id }))
                .await
            {
                Ok(answer) => answer,
                Err(err) => return self.brp_failure(err).await,
            };
            let answer = match serde_json::from_value::<JobAnswer>(answer) {
                Ok(answer) => answer,
                Err(err) => return failure(format!("`game.job.status` answered oddly: {err}")),
            };
            if answer.run != run {
                return failure(format!(
                    "the game restarted while job `{}` ran (run `{run}`, now `{}`); the job is \
                     gone with it",
                    status.id, answer.run
                ));
            }
            status = answer.job;
            tally = answer.log;
        }
    }

    async fn under_deadline(
        &self,
        wait: Duration,
        work: impl Future<Output = CallToolResult>,
        seen: &Mutex<Seen>,
    ) -> CallToolResult {
        let deadline = wait + self.hard_margin;
        match tokio::time::timeout(deadline, work).await {
            Ok(result) => result,
            Err(_) => {
                let seen = seen.lock().unwrap();
                failure(self.with_cause(match &seen.job {
                    Some((job, at)) => format!(
                        "the game stopped answering. Job `{}` was last {}, {:.1} seconds ago. \
                         Check it with job_wait.",
                        job.id,
                        job.state,
                        at.elapsed().as_secs_f32()
                    ),
                    None => format!(
                        "the game stopped answering: no answer within {} seconds. It may be hung \
                         or stopped at a breakpoint.",
                        deadline.as_secs_f32()
                    ),
                }))
            }
        }
    }

    /// A failed BRP call as a tool result, in words an agent can act on.
    async fn brp_failure(&self, error: anyhow::Error) -> CallToolResult {
        let Some(brp) = error.downcast_ref::<BrpError>() else {
            return failure(format!("{error:#}"));
        };
        if brp.code == METHOD_NOT_FOUND
            && (brp.method.starts_with("game.command.") || brp.method.starts_with("game.job."))
        {
            return failure(
                "this game binary predates the command registry; rebuild and restart it".to_owned(),
            );
        }
        let game_error: GameError = brp
            .data
            .clone()
            .and_then(|data| serde_json::from_value(data).ok())
            .unwrap_or_default();
        let game_error = GameError {
            message: brp.message.clone(),
            ..game_error
        };
        let log = game_error
            .run
            .as_deref()
            .and_then(|run| self.notice(run, game_error.log.as_ref()));
        if game_error.kind.as_deref() == Some("wrong_run") {
            self.catalog.refresh_and_notify(&self.brp).await;
            return failure(with_notice(
                format!(
                    "{} The tools have been refreshed; call again if it is still wanted.",
                    game_error.message
                ),
                log,
            ));
        }
        failure(with_notice(self.describe(&game_error), log))
    }

    /// `value` tagged with `run` and the notice for the tally `answer` carries.
    fn success(&self, value: Value, run: &str, answer: &Value) -> CallToolResult {
        success(value, run, self.notice(run, tally_of(answer).as_ref()))
    }

    /// "message (kind)", then the hint, with command names rewritten into callable ones.
    fn describe(&self, error: &GameError) -> String {
        let mut text = error.message.clone();
        if let Some(kind) = &error.kind {
            text.push_str(&format!(" ({kind})"));
        }
        if let Some(hint) = &error.hint {
            text.push_str(&format!(". {hint}"));
        }
        self.catalog.rewrite(&text)
    }
}

/// `wait_seconds` as a duration, clamped to 0..=50 s; `default` when absent or not a number.
fn wait_for(value: Option<&Value>, default: Duration) -> Duration {
    value.and_then(Value::as_f64).map_or(default, |seconds| {
        Duration::from_secs_f64(seconds.clamp(0.0, MAX_BLOCK.as_secs_f64()))
    })
}

/// The log tally beside an answer's `run`, from a game recent enough to send one.
fn tally_of(answer: &Value) -> Option<Tally> {
    serde_json::from_value(answer.get("log")?.clone()).ok()
}

/// A failure's text with the log notice after it.
fn with_notice(text: String, log: Option<LogNotice>) -> String {
    match log {
        Some(log) => format!("{text}\n\n{}", log.sentence()),
        None => text,
    }
}

/// A job still running when the wait ran out.
fn running(status: &JobStatus, run: &str, log: Option<LogNotice>) -> CallToolResult {
    let mut value = json!({
        "job": status.id,
        "state": status.state,
        "elapsed_seconds": status.elapsed_seconds,
        "note": format!(
            "still {} after {:.0} s; call job_wait with job `{}` to keep waiting, or job_cancel to \
             end it early",
            status.state, status.elapsed_seconds, status.id
        ),
    });
    if let Some(progress) = &status.progress {
        value["progress"] = progress.clone();
    }
    success(value, run, log)
}

/// `value` tagged with the run that produced it, as `pid`, the log notice and the staleness
/// warning when there are any. An object is tagged in place unless it has its own `pid`, `log` or
/// `warning`; anything else is nested under `result`, so nothing is overwritten.
pub(super) fn tagged(value: Value, run: &str, log: Option<LogNotice>) -> Value {
    let mut object = match value {
        Value::Object(map)
            if !["pid", "log", "warning"]
                .iter()
                .any(|key| map.contains_key(*key)) =>
        {
            map
        }
        other => {
            let mut map = Map::new();
            map.insert("result".into(), other);
            map
        }
    };
    object.insert("pid".into(), json!(run));
    if let Some(log) = log {
        object.insert("log".into(), json!(log));
    }
    if let Some(warning) = staleness::warning() {
        object.insert("warning".into(), json!(warning));
    }
    Value::Object(object)
}

fn success(value: Value, run: &str, log: Option<LogNotice>) -> CallToolResult {
    CallToolResult::structured(tagged(value, run, log))
}

/// A tool failure the agent sees whole. `GameServer::call_tool` adds the staleness warning.
pub(super) fn failure(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use rmcp::model::{CallToolResult, JsonObject};
    use serde_json::{Value, json};

    use rmcp::handler::server::wrapper::Parameters;

    use super::super::{
        GameServer,
        catalog::Listing,
        tests::{Responder, serve},
    };
    use super::CommandListParams;
    use crate::brp::BrpClient;

    type Seen = Arc<Mutex<Vec<(String, Value)>>>;

    /// One command of each kind, as `game.command.list` lists them.
    fn listing(run: &str) -> Value {
        json!({ "run": run, "commands": [
            {
                "name": "photo.take", "description": "Capture.", "kind": "job",
                "scope": "development", "read_only": false,
                "params": {
                    "type": "object",
                    "properties": { "path": { "type": "string" }, "source": { "$ref": "#/$defs/Source" } },
                    "$defs": { "Source": { "type": "string", "enum": ["window"] } },
                },
                "result": {},
            },
            {
                "name": "view.read", "description": "View.", "kind": "instant",
                "scope": "development", "read_only": true,
                "params": { "type": "object", "properties": {} }, "result": {},
            },
            {
                "name": "time.get", "description": "Shadowed.", "kind": "instant",
                "scope": "development", "read_only": true,
                "params": { "type": "object", "properties": {} }, "result": {},
            },
        ] })
    }

    /// A game that answers command calls. `answers` maps a method to its responses in order,
    /// repeating the last; `stall` never answers that method. Records every request.
    async fn game(
        answers: HashMap<&'static str, Vec<Value>>,
        stall: Option<&'static str>,
    ) -> (String, Seen) {
        let seen: Seen = Arc::default();
        let log = seen.clone();
        let counts: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
        let respond: Responder = Arc::new(move |_, request: &Value| {
            let method = request["method"].as_str().unwrap().to_owned();
            log.lock()
                .unwrap()
                .push((method.clone(), request["params"].clone()));
            if stall == Some(method.as_str()) {
                return None;
            }
            let nth = {
                let mut counts = counts.lock().unwrap();
                let count = counts.entry(method.clone()).or_default();
                *count += 1;
                *count - 1
            };
            Some(match answers.get(method.as_str()) {
                Some(list) => list[nth.min(list.len() - 1)].clone(),
                None => json!({ "error": { "code": -32601, "message": "Method not found" } }),
            })
        });
        (serve(respond).await, seen)
    }

    fn server(url: String) -> GameServer {
        let mut server = GameServer::new(BrpClient::new(url));
        server.cwd = Some("/project".into());
        server.hard_margin = Duration::from_millis(300);
        server
    }

    fn running(id: &str, frames: u32) -> Value {
        json!({ "result": { "run": "aaaaaaaa", "job": {
            "id": id, "command": "video.record", "state": "running", "elapsed_seconds": 0.5,
            "progress": { "frames": frames },
        } } })
    }

    fn succeeded(id: &str) -> Value {
        json!({ "result": { "run": "aaaaaaaa", "job": {
            "id": id, "command": "photo.take", "state": "succeeded", "elapsed_seconds": 0.1,
            "result": { "path": "/project/a.png", "width": 4, "height": 3 },
        } } })
    }

    fn text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|block| block.as_text().map(|text| text.text.clone()))
            .collect()
    }

    fn structured(result: &CallToolResult) -> Value {
        assert_ne!(result.is_error, Some(true), "{}", text(result));
        result.structured_content.clone().unwrap()
    }

    fn args(value: Value) -> JsonObject {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn commands_become_tools_unless_a_static_tool_has_the_name() {
        let server = GameServer::new(BrpClient::new("http://unused"));
        let first: Listing = serde_json::from_value(listing("aaaaaaaa")).unwrap();
        assert!(server.catalog.publish(first));
        let tools = server.catalog.tools();
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        assert_eq!(names, ["photo_take", "view_read"]);

        let screenshot = &tools[0];
        let schema = &screenshot.input_schema;
        assert!(schema["properties"]["wait_seconds"].is_object());
        assert_eq!(schema["$defs"]["Source"]["enum"], json!(["window"]));
        assert!(
            screenshot
                .description
                .as_ref()
                .unwrap()
                .contains("job_wait")
        );
        assert_eq!(
            tools[1].annotations.as_ref().unwrap().read_only_hint,
            Some(true)
        );
        assert!(
            tools[1].input_schema["properties"]
                .get("wait_seconds")
                .is_none()
        );

        // The same listing again changes nothing; a different description does.
        let again: Listing = serde_json::from_value(listing("bbbbbbbb")).unwrap();
        assert!(!server.catalog.publish(again));
        let mut changed = listing("bbbbbbbb");
        changed["commands"][1]["read_only"] = json!(false);
        let publish = |value: &Value| {
            server
                .catalog
                .publish(serde_json::from_value(value.clone()).unwrap())
        };
        assert!(publish(&changed), "annotations changed");
        assert!(!publish(&changed), "unchanged");
        changed["commands"][1]["description"] = json!("Another view.");
        assert!(publish(&changed), "description changed");
        assert!(!publish(&changed), "unchanged again");
    }

    #[test]
    fn hints_name_what_the_agent_can_call() {
        let server = GameServer::new(BrpClient::new("http://unused"));
        server
            .catalog
            .publish(serde_json::from_value(listing("aaaaaaaa")).unwrap());
        assert_eq!(
            server
                .catalog
                .rewrite("call `view.read` or `time.get` or `other.thing`"),
            "call `view_read` or `command_call` with command `time.get` or `other.thing`"
        );
    }

    #[tokio::test]
    async fn an_instant_result_is_tagged_with_the_run_that_answered() {
        let (url, seen) = game(
            HashMap::from([(
                "game.command.call",
                vec![json!({ "result": { "run": "aaaaaaaa", "result": { "view": "game", "pid": 1 } } })],
            )]),
            None,
        )
        .await;
        let result = server(url)
            .call_command(
                "view.read",
                args(json!({ "wait_seconds": 3 })),
                Some("aaaaaaaa".into()),
            )
            .await;
        assert_eq!(
            structured(&result),
            json!({ "pid": "aaaaaaaa", "result": { "view": "game", "pid": 1 } })
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "no separate pid read");
        assert_eq!(
            seen[0].1,
            json!({ "command": "view.read", "params": {}, "cwd": "/project", "run": "aaaaaaaa" })
        );
    }

    #[tokio::test]
    async fn a_job_is_followed_until_it_succeeds() {
        let (url, seen) = game(
            HashMap::from([
                ("game.command.call", vec![running("aaaaaaaa-1", 0)]),
                (
                    "game.job.status",
                    vec![running("aaaaaaaa-1", 3), succeeded("aaaaaaaa-1")],
                ),
            ]),
            None,
        )
        .await;
        let result = server(url)
            .call_command(
                "photo.take",
                args(json!({ "path": "a.png", "source": "window" })),
                None,
            )
            .await;
        assert_eq!(
            structured(&result),
            json!({ "pid": "aaaaaaaa", "path": "/project/a.png", "width": 4, "height": 3 })
        );
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen[0].1["params"],
            json!({ "path": "a.png", "source": "window" })
        );
        assert!(seen[0].1.get("run").is_none());
        assert_eq!(
            seen[1],
            ("game.job.status".into(), json!({ "job": "aaaaaaaa-1" }))
        );
    }

    #[tokio::test]
    async fn a_long_job_comes_back_running_with_a_note() {
        let (url, _) = game(
            HashMap::from([
                ("game.command.call", vec![running("aaaaaaaa-1", 0)]),
                ("game.job.status", vec![running("aaaaaaaa-1", 5)]),
            ]),
            None,
        )
        .await;
        let result = server(url)
            .call_command("video.record", args(json!({ "wait_seconds": 0.2 })), None)
            .await;
        let value = structured(&result);
        assert_eq!(value["job"], "aaaaaaaa-1");
        assert_eq!(value["state"], "running");
        assert_eq!(value["progress"]["frames"], 5);
        assert!(
            value["note"].as_str().unwrap().contains("job_wait"),
            "{value}"
        );
    }

    #[tokio::test]
    async fn a_failed_job_is_a_tool_error_with_kind_and_rewritten_hint() {
        let failed = json!({ "result": { "run": "aaaaaaaa", "job": {
            "id": "aaaaaaaa-1", "command": "photo.take", "state": "failed",
            "error": { "kind": "camera_inactive", "class": "failed", "message": "camera 5v0 is not active",
                       "hint": "call `view.read` first" },
        } } });
        let (url, _) = game(
            HashMap::from([
                ("game.command.call", vec![running("aaaaaaaa-1", 0)]),
                ("game.job.status", vec![failed]),
                (
                    "game.command.list",
                    vec![json!({ "result": listing("aaaaaaaa") })],
                ),
            ]),
            None,
        )
        .await;
        let server = server(url);
        server.catalog.refresh(&server.brp).await.unwrap();
        let result = server
            .call_command("photo.take", args(json!({})), None)
            .await;
        assert_eq!(result.is_error, Some(true));
        let text = text(&result);
        assert!(
            text.contains("camera 5v0 is not active (camera_inactive)"),
            "{text}"
        );
        assert!(text.contains("`view_read`"), "{text}");
    }

    #[tokio::test]
    async fn an_immediate_error_keeps_its_kind_and_hint() {
        let (url, _) = game(
            HashMap::from([(
                "game.command.call",
                vec![json!({ "error": { "code": -32602, "message": "no capture source `moon`",
                    "data": { "kind": "unknown_source", "class": "invalid", "run": "aaaaaaaa", "hint": "try window" } } })],
            )]),
            None,
        )
        .await;
        let result = server(url)
            .call_command("photo.take", args(json!({})), None)
            .await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            text(&result),
            "no capture source `moon` (unknown_source). try window"
        );
    }

    fn log(errors: u64, last_sequence: u64) -> Value {
        json!({ "errors": errors, "warnings": 0, "last_sequence": last_sequence,
                "last_error": format!("shader: broken {errors}") })
    }

    #[tokio::test]
    async fn a_result_carries_new_errors_once() {
        let instant = |errors| json!({ "result": { "run": "aaaaaaaa", "log": log(errors, 7), "result": { "view": "game" } } });
        let (url, _) = game(
            HashMap::from([(
                "game.command.call",
                vec![instant(1), instant(1), instant(2)],
            )]),
            None,
        )
        .await;
        let server = server(url);
        let call = || server.call_command("view.read", args(json!({})), None);

        let first = structured(&call().await);
        assert_eq!(
            first["log"],
            json!({ "new_errors": 1, "new_warnings": 0, "last_error": "shader: broken 1" })
        );
        assert_eq!(first["view"], "game");
        assert!(
            structured(&call().await).get("log").is_none(),
            "nothing new"
        );
        let third = structured(&call().await);
        assert_eq!(third["log"]["new_errors"], 1);
        assert_eq!(third["log"]["after_sequence"], 7);
    }

    #[tokio::test]
    async fn failures_carry_the_notice_in_their_text() {
        let failed = json!({ "result": { "run": "aaaaaaaa", "log": log(1, 3), "job": {
            "id": "aaaaaaaa-1", "command": "photo.take", "state": "failed",
            "error": { "kind": "camera_inactive", "class": "failed", "message": "dark" },
        } } });
        let error = json!({ "error": { "code": -32000, "message": "no",
            "data": { "kind": "nope", "class": "failed", "run": "aaaaaaaa", "log": log(2, 4) } } });
        let (url, _) = game(
            HashMap::from([
                ("game.command.call", vec![running("aaaaaaaa-1", 0), error]),
                ("game.job.status", vec![failed]),
            ]),
            None,
        )
        .await;
        let server = server(url);
        let job = server
            .call_command("photo.take", args(json!({})), None)
            .await;
        assert_eq!(job.is_error, Some(true));
        assert!(
            text(&job).contains("1 new error(s)") && text(&job).contains("shader: broken 1"),
            "{}",
            text(&job)
        );
        let brp = server
            .call_command("photo.take", args(json!({})), None)
            .await;
        assert!(
            text(&brp).contains("1 new error(s)") && text(&brp).contains("after_sequence 3"),
            "{}",
            text(&brp)
        );
    }

    #[tokio::test]
    async fn a_call_for_another_run_refreshes_and_is_not_replayed() {
        let (url, seen) = game(
            HashMap::from([
                (
                    "game.command.call",
                    vec![
                        json!({ "error": { "code": -32602, "message": "the game restarted",
                        "data": { "kind": "wrong_run", "class": "invalid", "run": "bbbbbbbb" } } }),
                    ],
                ),
                (
                    "game.command.list",
                    vec![json!({ "result": listing("bbbbbbbb") })],
                ),
            ]),
            None,
        )
        .await;
        let server = server(url);
        let result = server
            .call_command("view.read", args(json!({})), Some("aaaaaaaa".into()))
            .await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains("call again"), "{}", text(&result));
        assert_eq!(
            server
                .catalog
                .lookup("view_read")
                .map(|(_, run)| run)
                .as_deref(),
            Some("bbbbbbbb")
        );
        let calls = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| method == "game.command.call")
            .count();
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn an_old_game_is_told_to_rebuild() {
        let (url, _) = game(HashMap::new(), None).await;
        let result = server(url)
            .call_command("photo.take", args(json!({})), None)
            .await;
        assert!(
            text(&result).contains("predates the command registry"),
            "{}",
            text(&result)
        );
    }

    #[tokio::test]
    async fn gives_up_wherever_the_game_stops_answering() {
        for (stall, expected) in [
            ("game.command.call", "no answer within"),
            ("game.job.status", "was last running"),
        ] {
            let (url, _) = game(
                HashMap::from([("game.command.call", vec![running("aaaaaaaa-1", 0)])]),
                Some(stall),
            )
            .await;
            let result = server(url)
                .call_command("video.record", args(json!({ "wait_seconds": 0.1 })), None)
                .await;
            assert_eq!(result.is_error, Some(true), "{stall}");
            assert!(
                text(&result).contains(expected),
                "{stall}: {}",
                text(&result)
            );
        }
    }

    #[tokio::test]
    async fn the_list_tools_give_up_when_the_game_stops_answering() {
        for stall in ["game.command.list", "game.job.list"] {
            let (url, _) = game(HashMap::new(), Some(stall)).await;
            let server = server(url);
            let result = if stall == "game.command.list" {
                server
                    .command_list(Parameters(CommandListParams::default()))
                    .await
            } else {
                server.job_list().await
            }
            .unwrap();
            assert_eq!(result.is_error, Some(true), "{stall}");
            assert!(
                text(&result).contains("stopped answering"),
                "{stall}: {}",
                text(&result)
            );
        }
    }

    /// The tool and its run come from one snapshot: while refreshes publish runs A and B over and
    /// over, every lookup must pair a tool with the run that listed it.
    #[test]
    fn a_tool_is_never_paired_with_another_runs_id() {
        let server = GameServer::new(BrpClient::new("http://unused"));
        let catalog = server.catalog.clone();
        let listing_for = |run: &str| {
            let mut value = listing(run);
            // The description names the run, so a mismatched pair is detectable.
            value["commands"][1]["description"] = json!(run);
            serde_json::from_value::<Listing>(value).unwrap()
        };
        catalog.publish(listing_for("aaaaaaaa"));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let catalog = catalog.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut index = 0;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let run = if index % 2 == 0 {
                        "bbbbbbbb"
                    } else {
                        "aaaaaaaa"
                    };
                    catalog.publish(listing_for(run));
                    index += 1;
                }
            })
        };
        for _ in 0..20_000 {
            let (tool, run) = catalog.lookup("view_read").unwrap();
            assert_eq!(tool.command.description, run);
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        writer.join().unwrap();
    }

    #[tokio::test]
    async fn a_restart_under_a_running_job_is_reported() {
        let (url, _) = game(
            HashMap::from([
                ("game.command.call", vec![running("aaaaaaaa-1", 0)]),
                (
                    "game.job.status",
                    vec![json!({ "error": { "code": -32602, "message": "job `aaaaaaaa-1` is not from this run",
                        "data": { "kind": "unknown_job", "class": "invalid", "run": "bbbbbbbb" } } })],
                ),
            ]),
            None,
        )
        .await;
        let result = server(url)
            .call_command("video.record", args(json!({})), None)
            .await;
        assert!(
            text(&result).contains("not from this run"),
            "{}",
            text(&result)
        );
    }
}
