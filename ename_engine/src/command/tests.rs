use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::*;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EchoParams {
    /// Anything.
    text: String,
}

#[derive(Serialize, JsonSchema)]
struct Echoed {
    text: String,
}

fn echo(In(call): In<Call<EchoParams>>) -> Result<Echoed, CommandError> {
    Ok(Echoed {
        text: call.params.text,
    })
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PathParams {
    path: PathBuf,
}

fn resolve(In(call): In<Call<PathParams>>) -> Result<PathBuf, CommandError> {
    call.resolve_path(&call.params.path)
}

#[derive(Component)]
struct Work;

fn start(In(call): In<Call<NoParams>>, world: &mut World) -> Result<JobHandle<u32>, CommandError> {
    let work = world.spawn(Work).id();
    call.start_job(world, work, OnCancel::Despawn)
}

fn start_notified(
    In(call): In<Call<NoParams>>,
    world: &mut World,
) -> Result<JobHandle<u32>, CommandError> {
    let work = world.spawn(Work).id();
    call.start_job(world, work, OnCancel::Notify)
}

fn dev() -> Caller {
    Caller {
        scope: CommandScope::Development,
        cwd: None,
    }
}

fn safe() -> Caller {
    Caller {
        scope: CommandScope::Safe,
        cwd: None,
    }
}

fn app() -> App {
    let mut app = App::new();
    app.add_plugins(CommandPlugin)
        .register_command(
            CommandSpec::new("test.echo", "Echo.").scope(CommandScope::Safe),
            echo,
        )
        .register_command(CommandSpec::new("test.resolve", "Resolve."), resolve)
        .register_job_command(CommandSpec::new("test.start", "Start."), start)
        .register_job_command(CommandSpec::new("test.notify", "Notify."), start_notified);
    app
}

fn job_of(reply: Reply) -> JobId {
    match reply {
        Reply::Job(id) => id,
        Reply::Done(value) => panic!("expected a job, got {value}"),
    }
}

fn state(world: &World, job: &JobId) -> &'static str {
    world
        .resource::<Jobs>()
        .status(job, CommandScope::Development)
        .unwrap()
        .state
}

#[test]
fn calls_an_instant_command() {
    let mut app = app();
    let reply = app
        .world_mut()
        .call_command("test.echo", json!({ "text": "hi" }), dev())
        .unwrap();
    assert_eq!(reply, Reply::Done(json!({ "text": "hi" })));
}

#[test]
fn bad_params_name_the_field() {
    let mut app = app();
    for params in [json!({}), json!({ "text": "a", "extra": 1 }), json!(null)] {
        let err = app
            .world_mut()
            .call_command("test.echo", params.clone(), dev())
            .unwrap_err();
        assert_eq!(err.kind, "invalid_params", "{params}");
        assert_eq!(err.class, ErrorClass::Invalid);
    }
    let err = app
        .world_mut()
        .call_command("test.echo", json!({}), dev())
        .unwrap_err();
    assert!(err.message.contains("text"), "{}", err.message);
}

#[test]
fn null_params_count_as_an_empty_object() {
    let mut app = app();
    let reply = app
        .world_mut()
        .call_command("test.start", Value::Null, dev())
        .unwrap();
    assert!(matches!(reply, Reply::Job(_)));
}

#[test]
fn unknown_commands_and_scopes_are_refused() {
    let mut app = app();
    let err = app
        .world_mut()
        .call_command("test.nope", json!({}), dev())
        .unwrap_err();
    assert_eq!(err.kind, "unknown_command");
    assert!(err.hint.is_some());

    let err = app
        .world_mut()
        .call_command("test.start", json!({}), safe())
        .unwrap_err();
    assert_eq!(err.kind, "not_permitted");
    // A safe caller may run a safe command.
    app.world_mut()
        .call_command("test.echo", json!({ "text": "a" }), safe())
        .unwrap();
}

#[test]
fn paths_resolve_against_the_callers_directory() {
    let mut app = app();
    let world = app.world_mut();
    let call = |world: &mut World, path: &str, cwd: Option<&str>| {
        world.call_command(
            "test.resolve",
            json!({ "path": path }),
            Caller {
                scope: CommandScope::Development,
                cwd: cwd.map(PathBuf::from),
            },
        )
    };
    assert_eq!(
        call(world, "/abs/a.png", Some("/project")).unwrap(),
        Reply::Done(json!("/abs/a.png"))
    );
    assert_eq!(
        call(world, "shots/a.png", Some("/project")).unwrap(),
        Reply::Done(json!("/project/shots/a.png"))
    );
    let startup = std::env::current_dir().unwrap().join("a.png");
    assert_eq!(
        call(world, "a.png", None).unwrap(),
        Reply::Done(json!(startup))
    );
    let err = call(world, "a.png", Some("relative")).unwrap_err();
    assert_eq!(err.kind, "invalid_params");
}

#[test]
fn a_missing_working_directory_is_an_error_not_a_guess() {
    let mut app = app();
    app.world_mut().insert_resource(registry_startup_cwd(None));
    let err = app
        .world_mut()
        .call_command("test.resolve", json!({ "path": "a.png" }), dev())
        .unwrap_err();
    assert_eq!(err.kind, "no_working_directory");
    assert_eq!(
        app.world_mut()
            .call_command("test.resolve", json!({ "path": "/a.png" }), dev())
            .unwrap(),
        Reply::Done(json!(Path::new("/a.png")))
    );
}

fn registry_startup_cwd(cwd: Option<PathBuf>) -> registry::StartupCwd {
    registry::StartupCwd(cwd)
}

#[test]
fn lists_schemas_with_the_root_stripped_and_defs_kept() {
    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code, reason = "only the schema is read")]
    struct Nested {
        inner: Inner,
    }
    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code, reason = "only the schema is read")]
    enum Inner {
        A,
        B,
    }
    fn nested(In(_): In<Call<Nested>>) -> Result<(), CommandError> {
        Ok(())
    }
    let mut app = app();
    app.register_command(
        CommandSpec::new("test.nested", "Nested.")
            .read_only()
            .adjust_params_schema(|schema| {
                schema.insert("x-adjusted".into(), json!(true));
            }),
        nested,
    );
    let registry = app.world().resource::<CommandRegistry>();
    let info = registry.get("test.nested").unwrap();
    assert!(info.read_only);
    assert_eq!(info.kind, CommandKind::Instant);
    assert_eq!(info.params["type"], "object");
    assert!(info.params.get("title").is_none());
    assert_eq!(info.params["x-adjusted"], true);
    assert!(info.params["$defs"]["Inner"].is_object(), "{}", info.params);
    assert_eq!(
        info.params["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    let echo = registry.get("test.echo").unwrap();
    assert_eq!(
        echo.params["properties"]["text"]["description"],
        "Anything."
    );
    assert_eq!(registry.get("test.start").unwrap().kind, CommandKind::Job);
}

#[test]
fn a_result_schema_describes_what_is_serialized() {
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[allow(dead_code, reason = "only the schema is read")]
    struct Asymmetric {
        #[serde(rename(serialize = "written", deserialize = "read"))]
        field: u32,
    }
    #[derive(Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code, reason = "only the schema is read")]
    struct Params {
        #[serde(rename(serialize = "written", deserialize = "read"))]
        field: u32,
    }
    fn asymmetric(In(_): In<Call<Params>>) -> Result<Asymmetric, CommandError> {
        Ok(Asymmetric { field: 1 })
    }
    let mut app = app();
    app.register_command(
        CommandSpec::new("test.asymmetric", "Asymmetric."),
        asymmetric,
    );
    let info = app
        .world()
        .resource::<CommandRegistry>()
        .get("test.asymmetric")
        .unwrap()
        .clone();
    assert!(
        info.result["properties"].get("written").is_some(),
        "{}",
        info.result
    );
    assert!(
        info.params["properties"].get("read").is_some(),
        "{}",
        info.params
    );
}

#[test]
#[should_panic(expected = "namespace.action")]
fn a_name_without_one_dot_panics() {
    App::new().register_command(CommandSpec::new("echo", "Echo."), echo);
}

#[test]
#[should_panic(expected = "namespace.action")]
fn a_name_with_two_dots_panics() {
    App::new().register_command(CommandSpec::new("editor.view.set", "Echo."), echo);
}

#[test]
#[should_panic(expected = "registered twice")]
fn a_duplicate_name_panics() {
    App::new()
        .register_command(CommandSpec::new("test.echo", "Echo."), echo)
        .register_command(CommandSpec::new("test.echo", "Echo."), echo);
}

#[test]
#[should_panic(expected = "both become the tool name")]
fn names_that_share_a_tool_name_panic() {
    App::new()
        .register_command(CommandSpec::new("a.b_c", "Echo."), echo)
        .register_command(CommandSpec::new("a_b.c", "Echo."), echo);
}

#[test]
#[should_panic(expected = "must be a struct")]
fn non_object_params_panic() {
    fn number(In(_): In<Call<u32>>) -> Result<(), CommandError> {
        Ok(())
    }
    App::new().register_command(CommandSpec::new("test.number", "Number."), number);
}

#[test]
#[should_panic(expected = "may not define `wait_seconds`")]
fn reserved_params_panic() {
    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code, reason = "only the schema is read")]
    struct Waits {
        wait_seconds: f64,
    }
    fn waits(In(_): In<Call<Waits>>) -> Result<(), CommandError> {
        Ok(())
    }
    App::new().register_command(CommandSpec::new("test.waits", "Waits."), waits);
}

#[test]
fn a_handler_may_read_the_registry() {
    fn count(
        In(_): In<Call<NoParams>>,
        registry: Res<CommandRegistry>,
    ) -> Result<usize, CommandError> {
        Ok(registry.iter().count())
    }
    let mut app = app();
    app.register_command(CommandSpec::new("test.count", "Count."), count);
    assert_eq!(
        app.world_mut()
            .call_command("test.count", json!({}), dev())
            .unwrap(),
        Reply::Done(json!(5))
    );
}

#[test]
fn a_handler_calling_its_own_command_fails_instead_of_recursing() {
    fn recurse(In(_): In<Call<NoParams>>, world: &mut World) -> Result<(), CommandError> {
        world
            .call_command("test.recurse", json!({}), dev())
            .map(|_| ())
    }
    let mut app = app();
    app.register_command(CommandSpec::new("test.recurse", "Recurse."), recurse);
    let err = app
        .world_mut()
        .call_command("test.recurse", json!({}), dev())
        .unwrap_err();
    assert_eq!(err.kind, "handler_failed");
}

#[test]
fn a_job_runs_then_succeeds_and_is_kept_until_retention_ends() {
    let mut app = app();
    let world = app.world_mut();
    let job = job_of(world.call_command("test.start", json!({}), dev()).unwrap());
    assert!(job.as_str().starts_with(world.resource::<RunId>().as_str()));
    assert_eq!(state(world, &job), "running");

    world
        .resource_mut::<Jobs>()
        .set_progress(&job, json!({ "frames": 3 }));
    let status = world
        .resource::<Jobs>()
        .status(&job, CommandScope::Development)
        .unwrap();
    assert_eq!(status.progress, Some(json!({ "frames": 3 })));
    assert_eq!(status.command, "test.start");

    let handle = JobHandle::<u32>::new(job.clone());
    world.resource_mut::<Jobs>().finish(&handle, Ok(7));
    // A second outcome is ignored.
    world.resource_mut::<Jobs>().finish(&handle, Ok(8));
    let status = world
        .resource::<Jobs>()
        .status(&job, CommandScope::Development)
        .unwrap();
    assert_eq!(status.state, "succeeded");
    assert_eq!(status.result, Some(json!(7)));
    assert_eq!(status.progress, None);

    let now = Instant::now();
    world
        .resource_mut::<Jobs>()
        .expire(now + RETENTION - Duration::from_secs(1));
    assert_eq!(state(world, &job), "succeeded");
    world
        .resource_mut::<Jobs>()
        .expire(now + RETENTION + Duration::from_secs(1));
    let jobs = world.resource::<Jobs>();
    assert!(jobs.status(&job, CommandScope::Development).is_none());
    assert_eq!(jobs.unknown(&job).kind, "unknown_job");
}

#[test]
fn a_failed_job_keeps_its_error() {
    let mut app = app();
    let world = app.world_mut();
    let job = job_of(world.call_command("test.start", json!({}), dev()).unwrap());
    let error = CommandError::failed("camera_inactive", "dark").with_hint("switch");
    world
        .resource_mut::<Jobs>()
        .finish(&JobHandle::<u32>::new(job.clone()), Err(error.clone()));
    let status = world
        .resource::<Jobs>()
        .status(&job, CommandScope::Development)
        .unwrap();
    assert_eq!(status.state, "failed");
    assert_eq!(status.error, Some(error));
}

#[test]
fn removing_the_work_entity_cancels_the_job() {
    let mut app = app();
    let world = app.world_mut();
    let job = job_of(world.call_command("test.start", json!({}), dev()).unwrap());
    let work = world
        .query_filtered::<Entity, With<Work>>()
        .single(world)
        .unwrap();
    world.despawn(work);
    assert_eq!(state(world, &job), "cancelled");
}

#[test]
fn a_finished_job_is_not_cancelled_by_its_despawn() {
    let mut app = app();
    let world = app.world_mut();
    let job = job_of(world.call_command("test.start", json!({}), dev()).unwrap());
    world
        .resource_mut::<Jobs>()
        .finish(&JobHandle::<u32>::new(job.clone()), Ok(1));
    let work = world
        .query_filtered::<Entity, With<Work>>()
        .single(world)
        .unwrap();
    world.despawn(work);
    assert_eq!(state(world, &job), "succeeded");
}

#[test]
fn cancel_despawns_or_notifies() {
    let mut app = app();
    let world = app.world_mut();
    let despawned = job_of(world.call_command("test.start", json!({}), dev()).unwrap());
    let status = world
        .cancel_job(&despawned, CommandScope::Development)
        .unwrap();
    assert_eq!(status.state, "cancelled");
    assert!(status.cancel_requested);

    #[derive(Resource, Default)]
    struct Notified(Vec<Entity>);
    world.init_resource::<Notified>();
    // Reading `Jobs` while handling the request must not panic: `cancel_job` has let go of it.
    world.add_observer(
        |cancel: On<JobCancelRequested>, jobs: Res<Jobs>, mut notified: ResMut<Notified>| {
            assert!(!jobs.list(CommandScope::Development).is_empty());
            notified.0.push(cancel.entity);
        },
    );
    let notified = job_of(world.call_command("test.notify", json!({}), dev()).unwrap());
    let status = world
        .cancel_job(&notified, CommandScope::Development)
        .unwrap();
    assert_eq!(status.state, "running");
    assert!(status.cancel_requested);
    assert_eq!(world.resource::<Notified>().0.len(), 1);
    assert!(world.resource::<Jobs>().is_cancel_requested(&notified));

    // Cancelling a finished job changes nothing.
    assert_eq!(
        world
            .cancel_job(&despawned, CommandScope::Development)
            .unwrap()
            .state,
        "cancelled"
    );
}

#[test]
fn a_second_job_on_one_entity_is_refused() {
    fn twice(
        In(call): In<Call<NoParams>>,
        world: &mut World,
    ) -> Result<JobHandle<u32>, CommandError> {
        let work = world.spawn(Work).id();
        call.start_job::<u32>(world, work, OnCancel::Despawn)?;
        call.start_job(world, work, OnCancel::Despawn)
    }
    let mut app = app();
    app.register_job_command(CommandSpec::new("test.twice", "Twice."), twice);
    let err = app
        .world_mut()
        .call_command("test.twice", json!({}), dev())
        .unwrap_err();
    assert_eq!(err.kind, "job_conflict");
}

#[test]
fn jobs_above_the_callers_scope_are_invisible() {
    let mut app = app();
    let world = app.world_mut();
    let job = job_of(world.call_command("test.start", json!({}), dev()).unwrap());
    let jobs = world.resource::<Jobs>();
    assert!(jobs.status(&job, CommandScope::Safe).is_none());
    assert!(jobs.list(CommandScope::Safe).is_empty());
    assert_eq!(jobs.list(CommandScope::Development).len(), 1);
    let err = world.cancel_job(&job, CommandScope::Safe).unwrap_err();
    assert_eq!(err.kind, "unknown_job");
    assert_eq!(state(world, &job), "running");
}

#[test]
fn only_the_newest_finished_jobs_are_kept() {
    let mut jobs = Jobs::new(RunId::for_this_process());
    let ids: Vec<JobId> = (0..300)
        .map(|_| {
            let id = jobs.start(
                "test.start",
                CommandScope::Development,
                Entity::PLACEHOLDER,
                OnCancel::Despawn,
            );
            jobs.cancelled(&id);
            id
        })
        .collect();
    let running = jobs.start(
        "test.start",
        CommandScope::Development,
        Entity::PLACEHOLDER,
        OnCancel::Despawn,
    );
    jobs.expire(Instant::now());
    let kept = jobs.list(CommandScope::Development);
    assert_eq!(kept.len(), 257);
    assert!(jobs.status(&ids[0], CommandScope::Development).is_none());
    assert!(jobs.status(&ids[299], CommandScope::Development).is_some());
    assert!(jobs.status(&running, CommandScope::Development).is_some());
}

#[test]
fn an_id_from_another_run_says_so() {
    let jobs = Jobs::new(RunId::for_this_process());
    let err = jobs.unknown(&JobId::new("zzzzzzzz-1"));
    assert!(err.message.contains("not from this run"), "{}", err.message);
}
