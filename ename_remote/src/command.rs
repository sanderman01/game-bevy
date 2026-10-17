//! `game.command.*` and `game.job.*`: the command registry and its jobs, over BRP.
//!
//! The registry is `ename_engine::command`; this is only its side channel. The set of methods is
//! fixed: a crate adds functionality by registering a command, and it appears here without this
//! crate changing. Every answer carries `run`, read in the same handler as the result, so a client
//! never pairs a result with a run id fetched separately across a restart.

use std::path::PathBuf;

use bevy::{
    prelude::*,
    remote::{BrpError, BrpResult, error_codes},
};
use ename_engine::command::{
    Caller, CommandError, CommandRegistry, CommandScope, CommandWorldExt, ErrorClass, JobId, Jobs,
    Reply, RunId,
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};

pub const LIST_METHOD: &str = "game.command.list";
pub const CALL_METHOD: &str = "game.command.call";
pub const JOB_STATUS_METHOD: &str = "game.job.status";
pub const JOB_LIST_METHOD: &str = "game.job.list";
pub const JOB_CANCEL_METHOD: &str = "game.job.cancel";

/// JSON-RPC's server-defined range: the call was fine and the command failed.
pub const COMMAND_FAILED: i16 = -32000;

/// The side channel is development tooling, so it may call everything.
const SCOPE: CommandScope = CommandScope::Development;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CallParams {
    command: String,
    #[serde(default)]
    params: Value,
    /// The caller's working directory, for relative paths. Absolute.
    #[serde(default)]
    cwd: Option<PathBuf>,
    /// The run the caller expects. A different one refuses the call rather than running it.
    #[serde(default)]
    run: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobParams {
    job: JobId,
}

pub(crate) fn list(In(_): In<Option<Value>>, world: &mut World) -> BrpResult {
    let commands: Vec<_> = world
        .get_resource::<CommandRegistry>()
        .map(|registry| registry.iter().cloned().collect())
        .unwrap_or_default();
    answer(world, "commands", json!(commands))
}

pub(crate) fn call(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let CallParams {
        command,
        params,
        cwd,
        run: expected,
    } = parse(params, world)?;
    let run = run(world);
    if let Some(expected) = expected
        && expected != run
    {
        return Err(brp_error(
            CommandError::invalid(
                "wrong_run",
                format!(
                    "the call was meant for run `{expected}`, and this is run `{run}`: the game \
                     restarted, so nothing was run"
                ),
            ),
            world,
        ));
    }
    let caller = Caller { scope: SCOPE, cwd };
    match world.call_command(&command, params, caller) {
        Ok(Reply::Done(result)) => answer(world, "result", result),
        Ok(Reply::Job(job)) => {
            let jobs = world.resource::<Jobs>();
            match jobs.status(&job, SCOPE).ok_or_else(|| jobs.unknown(&job)) {
                Ok(status) => answer(world, "job", json!(status)),
                Err(err) => Err(brp_error(err, world)),
            }
        }
        Err(err) => Err(brp_error(err, world)),
    }
}

pub(crate) fn job_status(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let JobParams { job } = parse(params, world)?;
    let Some(jobs) = world.get_resource::<Jobs>() else {
        return Err(no_jobs(world));
    };
    match jobs.status(&job, SCOPE) {
        Some(status) => answer(world, "job", json!(status)),
        None => Err(brp_error(jobs.unknown(&job), world)),
    }
}

pub(crate) fn job_list(In(_): In<Option<Value>>, world: &mut World) -> BrpResult {
    let jobs = world
        .get_resource::<Jobs>()
        .map(|jobs| jobs.list(SCOPE))
        .unwrap_or_default();
    answer(world, "jobs", json!(jobs))
}

pub(crate) fn job_cancel(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let JobParams { job } = parse(params, world)?;
    match world.cancel_job(&job, SCOPE) {
        Ok(status) => answer(world, "job", json!(status)),
        Err(err) => Err(brp_error(err, world)),
    }
}

/// The method's own params. A malformed envelope is the caller's fault and gets the same
/// `kind`/`class`/`run` data as every other error here.
fn parse<T: DeserializeOwned>(params: Option<Value>, world: &World) -> Result<T, BrpError> {
    let params = params.unwrap_or(Value::Null);
    serde_json::from_value(params).map_err(|err| {
        brp_error(
            CommandError::invalid("invalid_params", format!("invalid params: {err}")),
            world,
        )
    })
}

fn run(world: &World) -> String {
    world
        .get_resource::<RunId>()
        .map_or_else(String::new, |run| run.as_str().to_owned())
}

/// What every answer and every error carries: the run, and the log tally read in the same
/// handler, after the work.
fn stamp(world: &World) -> Map<String, Value> {
    let mut stamp = Map::new();
    stamp.insert("run".into(), json!(run(world)));
    if let Some(tally) = crate::logs::tally(world) {
        stamp.insert("log".into(), json!(tally));
    }
    stamp
}

/// `{run, log, <key>: value}`.
fn answer(world: &World, key: &str, value: Value) -> BrpResult {
    let mut answer = stamp(world);
    answer.insert(key.into(), value);
    Ok(Value::Object(answer))
}

fn no_jobs(world: &World) -> BrpError {
    brp_error(
        CommandError::invalid("unknown_job", "this game has no command registry"),
        world,
    )
}

/// A command error on the wire: the class picks the code, and `data` keeps what a client
/// branches on.
pub(crate) fn brp_error(error: CommandError, world: &World) -> BrpError {
    let code = match error.class {
        ErrorClass::Invalid => error_codes::INVALID_PARAMS,
        ErrorClass::Failed => COMMAND_FAILED,
    };
    let mut data = stamp(world);
    data.insert("kind".into(), json!(error.kind));
    data.insert("class".into(), json!(error.class));
    if let Some(hint) = &error.hint {
        data.insert("hint".into(), json!(hint));
    }
    BrpError {
        code,
        message: error.message,
        data: Some(Value::Object(data)),
    }
}

#[cfg(test)]
mod tests {
    use bevy::remote::error_codes;
    use ename_engine::command::{
        Call, CommandAppExt, CommandPlugin, CommandSpec, JobHandle, NoParams, OnCancel,
    };
    use schemars::JsonSchema;
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct PathParams {
        path: PathBuf,
    }

    fn resolve(In(call): In<Call<PathParams>>) -> Result<PathBuf, CommandError> {
        call.resolve_path(&call.params.path)
    }

    fn fail(In(_): In<Call<NoParams>>) -> Result<(), CommandError> {
        Err(CommandError::failed("camera_inactive", "dark").with_hint("show it"))
    }

    #[derive(Component)]
    struct Work;

    fn start(
        In(call): In<Call<NoParams>>,
        world: &mut World,
    ) -> Result<JobHandle<u32>, CommandError> {
        let work = world.spawn(Work).id();
        call.start_job(world, work, OnCancel::Despawn)
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(CommandPlugin)
            .register_command(CommandSpec::new("test.resolve", "Resolve."), resolve)
            .register_command(CommandSpec::new("test.fail", "Fail."), fail)
            .register_job_command(CommandSpec::new("test.start", "Start."), start);
        app
    }

    fn method<M>(
        app: &mut App,
        method: impl IntoSystem<In<Option<Value>>, BrpResult, M> + 'static,
        params: Value,
    ) -> BrpResult {
        app.world_mut()
            .run_system_cached_with(method, Some(params))
            .unwrap()
    }

    fn run_of(app: &App) -> String {
        app.world().resource::<RunId>().as_str().to_owned()
    }

    #[test]
    fn lists_every_command_with_the_run() {
        let mut app = app();
        let run = run_of(&app);
        let answer = method(&mut app, list, json!({})).unwrap();
        assert_eq!(answer["run"], json!(run));
        let names: Vec<&str> = answer["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|command| command["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["test.fail", "test.resolve", "test.start"]);
        assert_eq!(answer["commands"][2]["kind"], "job");
        assert_eq!(answer["commands"][1]["params"]["type"], "object");
    }

    #[test]
    fn calls_with_the_callers_working_directory() {
        let mut app = app();
        let run = run_of(&app);
        let answer = method(
            &mut app,
            call,
            json!({ "command": "test.resolve", "params": { "path": "a.png" }, "cwd": "/project" }),
        )
        .unwrap();
        assert_eq!(answer, json!({ "run": run, "result": "/project/a.png" }));
    }

    #[test]
    fn a_job_answers_with_its_status() {
        let mut app = app();
        let answer = method(&mut app, call, json!({ "command": "test.start" })).unwrap();
        let job = answer["job"]["id"].as_str().unwrap().to_owned();
        assert!(job.starts_with(&run_of(&app)));
        assert_eq!(answer["job"]["state"], "running");

        let status = method(&mut app, job_status, json!({ "job": job })).unwrap();
        assert_eq!(status["job"]["state"], "running");
        let listed = method(&mut app, job_list, json!({})).unwrap();
        assert_eq!(listed["jobs"].as_array().unwrap().len(), 1);
        let cancelled = method(&mut app, job_cancel, json!({ "job": job })).unwrap();
        assert_eq!(cancelled["job"]["state"], "cancelled");
        assert_eq!(cancelled["run"], json!(run_of(&app)));
    }

    #[test]
    fn errors_keep_kind_class_hint_and_run() {
        let mut app = app();
        let run = run_of(&app);
        let err = method(&mut app, call, json!({ "command": "test.fail" })).unwrap_err();
        assert_eq!(err.code, COMMAND_FAILED);
        assert_eq!(err.message, "dark");
        assert_eq!(
            err.data,
            Some(
                json!({ "kind": "camera_inactive", "class": "failed", "hint": "show it", "run": run })
            )
        );

        let err = method(&mut app, call, json!({ "command": "test.nope" })).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert_eq!(err.data.unwrap()["kind"], "unknown_command");

        let err = method(&mut app, job_status, json!({ "job": "zzzzzzzz-1" })).unwrap_err();
        assert_eq!(err.data.unwrap()["kind"], "unknown_job");

        // A malformed envelope keeps the same data.
        for (handler, params) in [
            ("call", json!({ "params": {} })),
            ("call", json!({ "command": "test.fail", "extra": 1 })),
            ("status", json!({ "job": 7 })),
            ("cancel", json!({})),
        ] {
            let err = match handler {
                "call" => method(&mut app, call, params.clone()),
                "status" => method(&mut app, job_status, params.clone()),
                _ => method(&mut app, job_cancel, params.clone()),
            }
            .unwrap_err();
            assert_eq!(err.code, error_codes::INVALID_PARAMS, "{params}");
            assert_eq!(
                err.data.as_ref().map(|data| (&data["kind"], &data["run"])),
                Some((&json!("invalid_params"), &json!(run))),
                "{params}"
            );
        }
    }

    #[test]
    fn answers_and_errors_carry_the_log_tally() {
        let mut app = app();
        app.insert_resource(ename_engine::log::LogBuffer::default());
        let answer = method(&mut app, call, json!({ "command": "test.start" })).unwrap();
        assert_eq!(answer["log"], json!({ "errors": 0, "warnings": 0 }));
        let err = method(&mut app, call, json!({ "command": "test.fail" })).unwrap_err();
        assert_eq!(
            err.data.unwrap()["log"],
            json!({ "errors": 0, "warnings": 0 })
        );
    }

    #[test]
    fn a_call_for_another_run_is_refused_without_running() {
        let mut app = app();
        let err = method(
            &mut app,
            call,
            json!({ "command": "test.start", "run": "zzzzzzzz" }),
        )
        .unwrap_err();
        assert_eq!(err.data.unwrap()["kind"], "wrong_run");
        assert!(app.world().resource::<Jobs>().list(SCOPE).is_empty());

        let run = run_of(&app);
        method(
            &mut app,
            call,
            json!({ "command": "test.start", "run": run }),
        )
        .unwrap();
        assert_eq!(app.world().resource::<Jobs>().list(SCOPE).len(), 1);
    }
}
