//! The registry: named commands, their schemas, and the one way to call them.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
};

use bevy::prelude::*;
use schemars::{JsonSchema, Schema, generate::SchemaSettings};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use super::{
    CommandError, JobHandle, Jobs,
    jobs::{JobId, JobStatus, JobWork, OnCancel, cancel_job},
};

/// Params properties the MCP sidecar adds or the BRP call carries beside `params`. A command may
/// not define them.
const RESERVED_PARAMS: [&str; 2] = ["wait_seconds", "run"];

/// Longest command name: room for Claude Code's `mcp__game__` prefix inside the API's 64-character
/// tool-name limit.
const MAX_NAME_LEN: usize = 48;

/// Who may call a command. Ordered: a caller allowed `Development` may also call `Safe`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommandScope {
    /// Fine in a shipping console: reads state, changes presentation only.
    Safe,
    /// Development tooling: writes arbitrary files, drives the editor. Never offered to a player.
    Development,
}

/// Whether a command answers in the call or starts a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    Instant,
    Job,
}

/// How a command is described and who may call it. Built with [`CommandSpec::new`].
pub struct CommandSpec {
    name: &'static str,
    description: &'static str,
    scope: CommandScope,
    read_only: bool,
    adjust_params: Option<AdjustSchema>,
}

type AdjustSchema = Box<dyn FnOnce(&mut Schema)>;

impl CommandSpec {
    /// `name` is `namespace.action`, one dot, lowercase snake_case on both sides. `description`
    /// is one sentence: it is all an MCP client shows. Defaults to [`CommandScope::Development`]
    /// and not read-only.
    pub fn new(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            description,
            scope: CommandScope::Development,
            read_only: false,
            adjust_params: None,
        }
    }

    pub fn scope(mut self, scope: CommandScope) -> Self {
        self.scope = scope;
        self
    }

    /// No side effects. Becomes MCP's `readOnlyHint`; not a security boundary.
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    /// Edits the generated params schema before it is stored, for values known only at startup,
    /// such as the registered capture sources.
    pub fn adjust_params_schema(mut self, adjust: impl FnOnce(&mut Schema) + 'static) -> Self {
        self.adjust_params = Some(Box::new(adjust));
        self
    }
}

/// What the registry knows about one command, for listing.
#[derive(Clone, Debug, Serialize)]
pub struct CommandInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub kind: CommandKind,
    pub scope: CommandScope,
    pub read_only: bool,
    /// JSON schema (draft 2020-12) of the params object.
    pub params: Value,
    /// JSON schema of the result; for a job, of the job's final result.
    pub result: Value,
}

/// The caller of a command, as it identifies itself. Trusted: whoever builds one decides what it
/// may do.
#[derive(Clone, Debug)]
pub struct Caller {
    /// The most this caller may call, and the most whose jobs it may see.
    pub scope: CommandScope,
    /// The caller's working directory, for relative paths. Must be absolute. `None` means the
    /// game's, as it was when [`CommandPlugin`](super::CommandPlugin) built.
    pub cwd: Option<PathBuf>,
}

/// What a handler receives: its params and who called.
pub struct Call<P> {
    pub params: P,
    pub caller: Caller,
    command: &'static str,
    scope: CommandScope,
    base: Option<PathBuf>,
}

impl<P> Call<P> {
    /// Absolute paths are returned unchanged; relative ones are joined to the caller's working
    /// directory. Lexical only: nothing is canonicalized, because the file usually does not exist
    /// yet.
    pub fn resolve_path(&self, path: &Path) -> Result<PathBuf, CommandError> {
        if path.is_absolute() {
            return Ok(path.to_path_buf());
        }
        match &self.base {
            Some(base) => Ok(base.join(path)),
            None => Err(CommandError::invalid(
                "no_working_directory",
                format!(
                    "`{}` is relative, and there is no working directory to resolve it against",
                    path.display()
                ),
            )
            .with_hint("Pass an absolute path, or send `cwd` with the call.")),
        }
    }

    /// Starts a job for `work`, recording this call's command and scope on it.
    ///
    /// Fails with `job_conflict` if `work` already carries a job: replacing it would orphan the
    /// first. On failure `work` is untouched and still the caller's: despawn it if it was spawned
    /// for this job, or its work runs on with nothing tracking it.
    pub fn start_job<R>(
        &self,
        world: &mut World,
        work: Entity,
        on_cancel: OnCancel,
    ) -> Result<JobHandle<R>, CommandError> {
        let Ok(entity) = world.get_entity(work) else {
            return Err(CommandError::failed(
                "handler_failed",
                format!(
                    "the work entity {work} of `{}` does not exist",
                    self.command
                ),
            ));
        };
        if entity.contains::<JobWork>() {
            return Err(CommandError::failed(
                "job_conflict",
                format!("{work} already carries a job"),
            ));
        }
        let Some(mut jobs) = world.get_resource_mut::<Jobs>() else {
            return Err(CommandError::failed(
                "handler_failed",
                "jobs need `CommandPlugin`, which this app does not have",
            ));
        };
        let id = jobs.start(self.command, self.scope, work, on_cancel);
        world.entity_mut(work).insert(JobWork(id.clone()));
        Ok(JobHandle::new(id))
    }
}

/// What a call produced.
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    Done(Value),
    Job(JobId),
}

type Runner = Arc<dyn Fn(&mut World, Value, Call<()>) -> Result<Reply, CommandError> + Send + Sync>;

struct Entry {
    info: CommandInfo,
    run: Runner,
}

/// Every registered command. Filled while the app builds.
#[derive(Resource, Default)]
pub struct CommandRegistry {
    commands: BTreeMap<&'static str, Entry>,
    /// Tool name (the dot replaced) to command, so two names that map to one tool name are
    /// caught at registration.
    tool_names: HashMap<String, &'static str>,
}

impl CommandRegistry {
    /// Every command, by name.
    pub fn iter(&self) -> impl Iterator<Item = &CommandInfo> {
        self.commands.values().map(|entry| &entry.info)
    }

    pub fn get(&self, name: &str) -> Option<&CommandInfo> {
        self.commands.get(name).map(|entry| &entry.info)
    }

    fn insert(&mut self, info: CommandInfo, run: Runner) {
        let name = info.name;
        check_name(name);
        assert!(
            !self.commands.contains_key(name),
            "command `{name}` is registered twice"
        );
        let tool_name = name.replace('.', "_");
        if let Some(other) = self.tool_names.insert(tool_name.clone(), name) {
            panic!("commands `{other}` and `{name}` both become the tool name `{tool_name}`");
        }
        self.commands.insert(name, Entry { info, run });
    }
}

/// The game's working directory when `CommandPlugin` built, for callers that send none.
#[derive(Resource, Clone, Debug)]
pub(crate) struct StartupCwd(pub(crate) Option<PathBuf>);

fn check_name(name: &str) {
    let part_ok = |part: &str| {
        part.starts_with(|c: char| c.is_ascii_lowercase())
            && part
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    let valid = name.len() <= MAX_NAME_LEN
        && name
            .split_once('.')
            .is_some_and(|(namespace, action)| part_ok(namespace) && part_ok(action));
    assert!(
        valid,
        "command name `{name}` must be `namespace.action`: one dot, lowercase snake_case, at \
         most {MAX_NAME_LEN} characters"
    );
}

/// The draft 2020-12 schema of `T` as `settings` reads it (params as deserialized, results as
/// serialized, which differ under asymmetric serde attributes), with the root title and
/// description removed (the type's own
/// name and doc, noise to a client), the way rmcp generates its tools' schemas.
fn schema_for<T: JsonSchema>(settings: SchemaSettings) -> Schema {
    let mut schema = settings.into_generator().into_root_schema_for::<T>();
    schema.remove("title");
    schema.remove("description");
    schema
}

fn params_schema<P: JsonSchema>(spec: &mut CommandSpec) -> Value {
    let mut schema = schema_for::<P>(SchemaSettings::draft2020_12().for_deserialize());
    if let Some(adjust) = spec.adjust_params.take() {
        adjust(&mut schema);
    }
    assert_eq!(
        schema.get("type"),
        Some(&Value::from("object")),
        "the params of `{}` must be a struct: MCP needs an object schema",
        spec.name
    );
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for reserved in RESERVED_PARAMS {
            assert!(
                !properties.contains_key(reserved),
                "the params of `{}` may not define `{reserved}`, which callers use themselves",
                spec.name
            );
        }
    }
    schema.to_value()
}

fn parse<P: DeserializeOwned>(params: Value) -> Result<P, CommandError> {
    let params = if params.is_null() {
        Value::Object(Default::default())
    } else {
        params
    };
    serde_json::from_value(params)
        .map_err(|err| CommandError::invalid("invalid_params", err.to_string()))
}

fn handler_failed(name: &str, err: impl std::fmt::Display) -> CommandError {
    CommandError::failed("handler_failed", format!("`{name}` could not run: {err}"))
}

/// Registering commands on an [`App`].
pub trait CommandAppExt {
    /// A command that answers in the call.
    fn register_command<P, R, M>(
        &mut self,
        spec: CommandSpec,
        handler: impl IntoSystem<In<Call<P>>, Result<R, CommandError>, M> + 'static,
    ) -> &mut Self
    where
        P: DeserializeOwned + JsonSchema + Send + 'static,
        R: Serialize + JsonSchema + 'static;

    /// A command that starts a job with [`Call::start_job`] and returns its handle. `R` is the
    /// job's final result.
    fn register_job_command<P, R, M>(
        &mut self,
        spec: CommandSpec,
        handler: impl IntoSystem<In<Call<P>>, Result<JobHandle<R>, CommandError>, M> + 'static,
    ) -> &mut Self
    where
        P: DeserializeOwned + JsonSchema + Send + 'static,
        R: Serialize + JsonSchema + 'static;
}

impl CommandAppExt for App {
    fn register_command<P, R, M>(
        &mut self,
        mut spec: CommandSpec,
        handler: impl IntoSystem<In<Call<P>>, Result<R, CommandError>, M> + 'static,
    ) -> &mut Self
    where
        P: DeserializeOwned + JsonSchema + Send + 'static,
        R: Serialize + JsonSchema + 'static,
    {
        let info = CommandInfo {
            name: spec.name,
            description: spec.description,
            kind: CommandKind::Instant,
            scope: spec.scope,
            read_only: spec.read_only,
            params: params_schema::<P>(&mut spec),
            result: schema_for::<R>(SchemaSettings::draft2020_12().for_serialize()).to_value(),
        };
        let world = self.world_mut();
        let id = world.register_system(handler);
        let name = spec.name;
        let run: Runner = Arc::new(move |world, params, call| {
            let call = call.with_params(parse::<P>(params)?);
            let result = world
                .run_system_with(id, call)
                .map_err(|err| handler_failed(name, err))??;
            serde_json::to_value(result)
                .map(Reply::Done)
                .map_err(|err| handler_failed(name, err))
        });
        world
            .get_resource_or_init::<CommandRegistry>()
            .insert(info, run);
        self
    }

    fn register_job_command<P, R, M>(
        &mut self,
        mut spec: CommandSpec,
        handler: impl IntoSystem<In<Call<P>>, Result<JobHandle<R>, CommandError>, M> + 'static,
    ) -> &mut Self
    where
        P: DeserializeOwned + JsonSchema + Send + 'static,
        R: Serialize + JsonSchema + 'static,
    {
        let info = CommandInfo {
            name: spec.name,
            description: spec.description,
            kind: CommandKind::Job,
            scope: spec.scope,
            read_only: spec.read_only,
            params: params_schema::<P>(&mut spec),
            result: schema_for::<R>(SchemaSettings::draft2020_12().for_serialize()).to_value(),
        };
        let world = self.world_mut();
        let id = world.register_system(handler);
        let name = spec.name;
        let run: Runner = Arc::new(move |world, params, call| {
            let call = call.with_params(parse::<P>(params)?);
            let job = world
                .run_system_with(id, call)
                .map_err(|err| handler_failed(name, err))??;
            Ok(Reply::Job(job.id().clone()))
        });
        world
            .get_resource_or_init::<CommandRegistry>()
            .insert(info, run);
        self
    }
}

impl Call<()> {
    fn with_params<P>(self, params: P) -> Call<P> {
        Call {
            params,
            caller: self.caller,
            command: self.command,
            scope: self.scope,
            base: self.base,
        }
    }
}

/// Calling commands and cancelling jobs on a [`World`]. The one entry point both the BRP side
/// channel and a console use.
pub trait CommandWorldExt {
    /// Checks the caller, deserializes `params` (`null` counts as `{}`) and runs the handler.
    fn call_command(
        &mut self,
        name: &str,
        params: Value,
        caller: Caller,
    ) -> Result<Reply, CommandError>;

    /// Cancels a job as its [`OnCancel`] says, and returns its status afterwards. Jobs above
    /// `scope` are unknown to it.
    fn cancel_job(&mut self, job: &JobId, scope: CommandScope) -> Result<JobStatus, CommandError>;
}

impl CommandWorldExt for World {
    fn call_command(
        &mut self,
        name: &str,
        params: Value,
        caller: Caller,
    ) -> Result<Reply, CommandError> {
        if let Some(cwd) = &caller.cwd
            && !cwd.is_absolute()
        {
            return Err(CommandError::invalid(
                "invalid_params",
                format!("`cwd` must be absolute, not `{}`", cwd.display()),
            ));
        }
        // Cloned out, so the handler may read the registry itself.
        let (run, command, scope) = {
            let entry = self
                .get_resource::<CommandRegistry>()
                .and_then(|registry| registry.commands.get(name))
                .ok_or_else(|| unknown_command(name))?;
            (entry.run.clone(), entry.info.name, entry.info.scope)
        };
        if scope > caller.scope {
            return Err(CommandError::invalid(
                "not_permitted",
                format!("`{name}` is a {scope:?} command, and this caller may not run those"),
            ));
        }
        let base = caller.cwd.clone().or_else(|| {
            self.get_resource::<StartupCwd>()
                .and_then(|cwd| cwd.0.clone())
        });
        let call = Call {
            params: (),
            caller,
            command,
            scope,
            base,
        };
        run(self, params, call)
    }

    fn cancel_job(&mut self, job: &JobId, scope: CommandScope) -> Result<JobStatus, CommandError> {
        if !self.contains_resource::<Jobs>() {
            return Err(CommandError::invalid(
                "unknown_job",
                format!("no job `{job}`: this app has no `CommandPlugin`"),
            ));
        }
        cancel_job(self, job, scope)
    }
}

fn unknown_command(name: &str) -> CommandError {
    CommandError::invalid(
        "unknown_command",
        format!("no command `{name}` in this build"),
    )
    .with_hint(
        "This build does not register it. A cargo feature may compile it out, or the binary may \
         not add the plugin that provides it. Listing the commands shows what this build has.",
    )
}
