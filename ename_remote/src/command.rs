//! `game.command.*` and `game.job.*`: the command registry and its jobs, over BRP.
//!
//! The registry is `ename_engine::command`; this is only its side channel. The set of methods is
//! fixed: a crate adds functionality by registering a command, and it appears here without this
//! crate changing. Every answer carries `run`, read in the same handler as the result, so a client
//! never pairs a result with a run id fetched separately across a restart.

use std::path::PathBuf;

use bevy::{
    prelude::*,
    remote::{BrpError, BrpResult, builtin_methods::parse_some, error_codes},
};
use ename_engine::command::{
    Caller, CommandError, CommandRegistry, CommandScope, CommandWorldExt, ErrorClass, JobId, Jobs,
    Reply, RunId,
};
use serde::Deserialize;
use serde_json::{Value, json};

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
    crate::to_value(json!({ "run": run(world), "commands": commands }))
}

pub(crate) fn call(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let CallParams {
        command,
        params,
        cwd,
        run: expected,
    } = parse_some(params)?;
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
            &run,
        ));
    }
    let caller = Caller { scope: SCOPE, cwd };
    match world.call_command(&command, params, caller) {
        Ok(Reply::Done(result)) => crate::to_value(json!({ "run": run, "result": result })),
        Ok(Reply::Job(job)) => {
            let jobs = world.resource::<Jobs>();
            let status = jobs.status(&job, SCOPE).ok_or_else(|| jobs.unknown(&job));
            match status {
                Ok(status) => crate::to_value(json!({ "run": run, "job": status })),
                Err(err) => Err(brp_error(err, &run)),
            }
        }
        Err(err) => Err(brp_error(err, &run)),
    }
}

pub(crate) fn job_status(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let JobParams { job } = parse_some(params)?;
    let run = run(world);
    let Some(jobs) = world.get_resource::<Jobs>() else {
        return Err(no_jobs(&run));
    };
    match jobs.status(&job, SCOPE) {
        Some(status) => crate::to_value(json!({ "run": run, "job": status })),
        None => Err(brp_error(jobs.unknown(&job), &run)),
    }
}

pub(crate) fn job_list(In(_): In<Option<Value>>, world: &mut World) -> BrpResult {
    let run = run(world);
    let jobs = world
        .get_resource::<Jobs>()
        .map(|jobs| jobs.list(SCOPE))
        .unwrap_or_default();
    crate::to_value(json!({ "run": run, "jobs": jobs }))
}

pub(crate) fn job_cancel(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let JobParams { job } = parse_some(params)?;
    let run = run(world);
    match world.cancel_job(&job, SCOPE) {
        Ok(status) => crate::to_value(json!({ "run": run, "job": status })),
        Err(err) => Err(brp_error(err, &run)),
    }
}

fn run(world: &World) -> String {
    world
        .get_resource::<RunId>()
        .map_or_else(String::new, |run| run.as_str().to_owned())
}

fn no_jobs(run: &str) -> BrpError {
    brp_error(
        CommandError::invalid("unknown_job", "this game has no command registry"),
        run,
    )
}

/// A command error on the wire: the class picks the code, and `data` keeps what a client
/// branches on.
pub(crate) fn brp_error(error: CommandError, run: &str) -> BrpError {
    let code = match error.class {
        ErrorClass::Invalid => error_codes::INVALID_PARAMS,
        ErrorClass::Failed => COMMAND_FAILED,
    };
    let mut data = json!({ "kind": error.kind, "class": error.class, "run": run });
    if let Some(hint) = &error.hint {
        data["hint"] = json!(hint);
    }
    BrpError {
        code,
        message: error.message,
        data: Some(data),
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
