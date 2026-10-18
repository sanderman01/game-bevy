//! The tool catalogue.
//!
//! Design rules, in the order they matter:
//!
//! 1. Do not mirror BRP. A tool that is a one-to-one rename of a BRP method is a bad tool.
//! 2. Never reimplement reflection. Component and resource values are read and written by the
//!    game's `game.components.*` and `game.resources.*` methods, which resolve names, types and
//!    paths. No tool parses a property path or a type name itself.
//! 3. Anything settable is gettable.
//! 4. A tool returns data or it fails. There is no `{"success": false}` for the agent to
//!    remember to check.
//! 5. A tool's name and one sentence are all the model gets. Write them that way.
//!
//! Static tool names are prefixed by group (`world_`, `time_`, `log_`, `command_`, `job_`). The
//! game's commands are added at runtime as tools of their own, named after the command with the
//! dot replaced (see [`catalog`]); a static name always wins over a command's.
//!
//! Every tool failure reaches the agent as a tool result marked `isError`, never as a JSON-RPC
//! error: a client may show only an error's message and drop its data.

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use rmcp::{
    ErrorData, Json, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, tool::ToolCallContext, wrapper::Parameters},
    model::{
        CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
        ListToolsResult, PaginatedRequestParams, ProtocolVersion, ResultType, ServerCapabilities,
        ServerInfo, SubscriptionFilter, Tool,
    },
    schemars,
    service::{NotificationContext, RequestContext, SubscriptionContext},
    tool, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod catalog;
mod commands;
mod lifecycle;
#[cfg(test)]
mod session_tests;

use catalog::Catalog;
pub use lifecycle::Lifecycle;

use crate::{
    brp::{BrpClient, Endpoint},
    entity::{EntitySelector, ResolvedEntity},
    notice::{LogNotice, NoticeState, Tally},
    staleness,
};

/// The MCP server. One instance per client connection, one game endpoint behind it.
#[derive(Clone)]
pub struct GameServer {
    /// The game this session talks to, followed between calls.
    endpoint: Arc<Endpoint>,
    /// This call's snapshot of `endpoint`: every request of one tool call goes to one game.
    brp: BrpClient,
    /// The game launcher, when the sidecar's working directory is a workspace that names its
    /// editor.
    lifecycle: Option<Arc<Lifecycle>>,
    tool_router: ToolRouter<Self>,
    /// The game's commands as tools, and who to tell when they change.
    catalog: Arc<Catalog>,
    /// Sent with every command call: the agent's relative paths mean this directory.
    cwd: Option<PathBuf>,
    /// Room past a tool's own wait before it gives up on a game that stopped answering.
    hard_margin: Duration,
    /// The log tally each run has already reported to the agent.
    notices: Arc<NoticeState>,
}

impl GameServer {
    /// A server that always talks to `brp`.
    #[cfg(test)]
    pub fn new(brp: BrpClient) -> Self {
        Self::with_endpoint(Endpoint::fixed(brp), None)
    }

    pub fn with_endpoint(endpoint: Endpoint, lifecycle: Option<Lifecycle>) -> Self {
        let tool_router = Self::tool_router() + Self::commands_router() + Self::lifecycle_router();
        let static_tools = tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned());
        let endpoint = Arc::new(endpoint);
        Self {
            brp: endpoint.snapshot(),
            endpoint,
            lifecycle: lifecycle.map(Arc::new),
            catalog: Arc::new(Catalog::new(static_tools)),
            tool_router,
            cwd: std::env::current_dir().ok(),
            hard_margin: commands::HARD_MARGIN,
            notices: Arc::default(),
        }
    }

    /// This server with the endpoint snapshotted for one tool call.
    fn for_call(&self) -> Self {
        Self {
            brp: self.endpoint.snapshot(),
            ..self.clone()
        }
    }

    /// Attaches what the agent needs to know about this call to a finished tool result.
    ///
    /// The run id is read after the work rather than before, so it names the process that
    /// actually served the request. One extra loopback call per tool; the handler reads a
    /// single resource.
    async fn tag<T>(&self, result: T) -> ToolResult<T> {
        let pid: Pid = self
            .brp
            .call("game.pid.get", json!({}))
            .await
            .map_err(fail)?;
        Ok(Json(Tagged {
            log: self.notice(&pid.pid, pid.log.as_ref()),
            pid: pid.pid,
            warning: staleness::warning(),
            result,
        }))
    }

    /// The log notice for an answer from `run`, if the game logged anything new. Advances the
    /// run's mark, so call it only for a result the agent will see.
    fn notice(&self, run: &str, tally: Option<&Tally>) -> Option<LogNotice> {
        let tally = tally?;
        if let Some(error) = &tally.last_error {
            self.brp.remember_error(run, error);
        }
        self.notices.notice(run, tally)
    }

    /// The log notice as a sentence for a static tool that failed, from a `game.pid.get` read
    /// bounded like the failed call. `None` when the game cannot say.
    async fn failure_notice(&self) -> Option<String> {
        let read = self.brp.call::<Pid>("game.pid.get", json!({}));
        let pid = tokio::time::timeout(FAILURE_NOTICE_WAIT, read)
            .await
            .ok()?
            .ok()?;
        Some(self.notice(&pid.pid, pid.log.as_ref())?.sentence())
    }
}

/// How long a failed static tool waits to read the log tally. Short: a game that cannot answer
/// already said so in the failure.
const FAILURE_NOTICE_WAIT: Duration = Duration::from_secs(2);

/// What `game.pid.get` answers.
#[derive(Deserialize)]
struct Pid {
    pid: String,
    #[serde(default)]
    log: Option<Tally>,
}

/// Tool failures reach the agent as failures, with the sentence that explains them.
/// [`warn_on_failure`] adds the staleness warning.
fn fail(error: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(format!("{error:#}"), None)
}

/// Adds the full staleness warning to a failed result. Every tool failure passes here, rmcp's
/// own argument errors included, because that is where a stale build shows up: as a schema the
/// running code predates, or an answer it cannot parse.
fn warn_on_failure(mut response: CallToolResponse) -> CallToolResponse {
    if let CallToolResponse::Complete(result) = &mut response
        && result.is_error == Some(true)
        && let Some(warning) = staleness::failure_warning()
    {
        result.content.push(ContentBlock::text(warning));
    }
    response
}

type ToolResult<T> = Result<Json<Tagged<T>>, ErrorData>;

/// Every tool result, wrapped with what the agent should know about the answer rather than from
/// it. A future fact about the call belongs here rather than in each tool's own result type.
///
/// Names are kept short and fields absent when they have nothing to say, because this rides on
/// every response.
#[derive(Serialize, schemars::JsonSchema)]
pub struct Tagged<T> {
    /// Identifies this run of the game process. A different value from the last call means the
    /// game restarted: re-resolve entities by name, because the ids are stale.
    pid: String,
    /// New errors and warnings the game logged since the last result that reported any. Absent
    /// when there are none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    log: Option<LogNotice>,
    /// Something about this server, not about the result, that needs acting on. Absent in the
    /// ordinary case, which is all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
    #[serde(flatten)]
    result: T,
}

// ---------------------------------------------------------------------------------------------
// world_query

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct QueryParams {
    /// Case-insensitive substring of the entity's name. Entities with no name never match.
    #[serde(default)]
    pub name_contains: Option<String>,
    /// Component types, e.g. "RigidBody". A short name matches only types with that short name
    /// (any case); anything else is a case-insensitive substring of the type path. An entity
    /// must have a component matching every one. `matched_types` says what each matched.
    #[serde(default)]
    pub with_components: Vec<String>,
    /// Restricts the result to descendants of this entity.
    #[serde(default)]
    pub parent: Option<EntitySelector>,
    /// Include the entities Bevy uses to store resources and observers. Off by default: they
    /// are most of the world and none of them are scene content.
    #[serde(default)]
    pub include_internal: bool,
    /// Defaults to 50. When cut, `name_counts` says which names fill the rest.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Extra columns per entity: "parent", "children" (a count), "components" (short names).
    #[serde(default)]
    pub fields: Vec<QueryField>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryField {
    Parent,
    Children,
    Components,
}

// ---------------------------------------------------------------------------------------------
// world_overview

#[derive(Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct OverviewParams {
    /// Show only this entity's subtree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<EntitySelector>,
    /// How many named levels deep. Defaults to 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    /// Most entities listed. Defaults to 80.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

// ---------------------------------------------------------------------------------------------
// world_list_components

/// One entity by `name` or `entity`, or several under `entities`.
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ListComponentsParams {
    #[serde(flatten)]
    pub selector: EntitySelector,
    /// Several entities in one call, each by `name` or `entity`. Instead of `name`/`entity`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<EntitySelector>,
}

// ---------------------------------------------------------------------------------------------
// world_get_components and world_get_position

/// Which entity, and nothing else. For the tools whose whole input is the selector.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EntityParams {
    #[serde(flatten)]
    pub selector: EntitySelector,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct GetComponentsParams {
    #[serde(flatten)]
    pub selector: EntitySelector,
    /// Component types by short name ("Transform") or path; "Transform.rotation" reads one
    /// field.
    pub components: Vec<String>,
    /// A value bigger than this, serialized, comes back as its size and field names, to read by
    /// path. Defaults to 8000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<usize>,
}

/// The three components a position is made of. Hard-coded because the sidecar does not link
/// Bevy, and they are the same three `game.position.get` computes from.
const CELL_COORD: &str = "big_space::grid::cell::CellCoord";
const TRANSFORM: &str = "bevy_transform::components::transform::Transform";
const GLOBAL_TRANSFORM: &str = "bevy_transform::components::global_transform::GlobalTransform";

#[derive(Serialize, schemars::JsonSchema)]
pub struct EntityPosition {
    #[serde(flatten)]
    identity: ResolvedEntity,
    /// Absolute metres, x/y/z, Y up. The only one of these fields that names a fixed world
    /// point. Absent when the entity is under no grid and so has no world position.
    position: Option<[f64; 3]>,
    /// The entity holding the grid `position` is expressed in.
    grid: Option<u64>,
    /// `CellCoord`: which grid cell the entity sits in. Absent when it has none, which counts
    /// as the grid's origin cell.
    cell: Option<Value>,
    /// `Transform`: the offset within the cell, from an origin that moves with the camera. On
    /// its own it names a different world point from one frame to the next.
    transform: Option<Value>,
    /// `GlobalTransform`: the same offset composed down the hierarchy. Still relative to the
    /// moving origin, so still not a world position.
    global_transform: Option<Value>,
    /// Any of the three the entity has but whose value would not serialize, with the reason. A
    /// component listed here is null above because reflection could not show it, not because it
    /// is missing.
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    unreadable: HashMap<String, Value>,
}

/// What `game.components.get` answers.
#[derive(Default, Deserialize)]
#[serde(default)]
struct ComponentValues {
    entity: u64,
    name: Option<String>,
    components: HashMap<String, Value>,
    unreadable: HashMap<String, Value>,
}

// ---------------------------------------------------------------------------------------------
// registry_schema

/// Everything is optional, but a call with no filter at all returns whatever `limit` allows out
/// of more than a thousand types. Name the types you actually need.
#[derive(Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct RegistrySchemaParams {
    /// Types by short name ("RigidBody"), path suffix ("grid::Grid") or full path. A short name
    /// several types share returns them all.
    #[serde(default)]
    pub types: Vec<String>,
    /// Case-insensitive substring of the type path, for when the name is not known yet.
    #[serde(default)]
    pub contains: Option<String>,
    /// Only types from these crates, e.g. "avian3d" or "ename_engine".
    #[serde(default)]
    pub with_crates: Vec<String>,
    /// Exclude types from these crates.
    #[serde(default)]
    pub without_crates: Vec<String>,
    /// Only component types.
    #[serde(default)]
    pub components_only: bool,
    /// Defaults to 25.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Also return the upstream JSON schema of each type. Large; describes reflection
    /// structure, not the wire form.
    #[serde(default)]
    pub full_schema: bool,
}

/// What `game.registry.describe` answers, passed through.
#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct RegistrySchema {
    /// Each type: `type_path`, `short`, `kind`, `component`, `resource`, `shape` (how to write
    /// it, one line) and `example` (a value the game accepts), plus `fields` or `variants`.
    types: Vec<Value>,
    /// Types that matched but were cut by `limit`. Narrow the filters if this is not zero.
    truncated: usize,
    /// Names in `types` that match no registered type: nothing can read or write them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unregistered: Vec<String>,
    /// Names in `types` that are components without reflection: they can be listed and removed,
    /// not read or written.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    not_reflected: Vec<String>,
}

// ---------------------------------------------------------------------------------------------
// mutation params

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SpawnEntityParams {
    /// The new entity's `Name`. Required: an unnamed entity can only ever be addressed by an id
    /// that expires when the game restarts.
    pub name: String,
    /// Component type (short name or path) to value, e.g. {"RigidBody": "Dynamic", "NoAutoMass":
    /// {}}.
    #[serde(default)]
    pub components: serde_json::Map<String, Value>,
    /// Absolute metres. Placing the entity also parents it to the world grid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f64; 3]>,
    /// Parent to attach to. Defaults to the world grid when `position` is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<EntitySelector>,
    /// Refuse values that need fixing instead of fixing them.
    #[serde(default)]
    pub strict: bool,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct InsertComponentParams {
    #[serde(flatten)]
    pub selector: EntitySelector,
    /// Component type (short name or path) to value, e.g. {"ConstantTorque": [0, 1, 0]}.
    pub components: serde_json::Map<String, Value>,
    /// Refuse values that need fixing instead of fixing them.
    #[serde(default)]
    pub strict: bool,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct RemoveComponentParams {
    #[serde(flatten)]
    pub selector: EntitySelector,
    /// Component types, by short name or path.
    pub components: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct MutateComponentParams {
    #[serde(flatten)]
    pub selector: EntitySelector,
    /// Component type by short name or path, e.g. "Transform" or "DirectionalLight".
    pub component: String,
    /// Field path within the component, e.g. "rotation" or "scale.x". Empty sets the whole
    /// component.
    #[serde(default)]
    pub path: String,
    /// The new value in the shape registry_schema shows, e.g. 0.5, "Hidden", [0, 1, 0], {}.
    pub value: Value,
    /// Refuse values that need fixing instead of fixing them.
    #[serde(default)]
    pub strict: bool,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct GetResourcesParams {
    /// Resource types by short name or path, optionally with a field path: "DebugOverlays" or
    /// "DebugOverlays.physics".
    pub resources: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct MutateResourceParams {
    /// Resource type by short name or path.
    pub resource: String,
    /// Field path within the resource, e.g. "physics". Empty sets the whole resource.
    #[serde(default)]
    pub path: String,
    /// The new value in the shape registry_schema shows, e.g. true, 0.5, "Hidden".
    pub value: Value,
    /// Refuse values that need fixing instead of fixing them.
    #[serde(default)]
    pub strict: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReparentEntityParams {
    #[serde(flatten)]
    pub selector: EntitySelector,
    /// The new parent. Omit to detach the entity from its current parent.
    #[serde(default)]
    pub parent: Option<EntitySelector>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetPositionParams {
    #[serde(flatten)]
    pub selector: EntitySelector,
    /// Absolute metres, x/y/z. Y is up.
    pub position: [f64; 3],
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct PositionResult {
    #[serde(flatten)]
    identity: ResolvedEntity,
    /// Absolute metres. Absent when the entity was created without a position.
    position: Option<[f64; 3]>,
}

// ---------------------------------------------------------------------------------------------
// time control and logs

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetTimeParams {
    /// "pause" freezes virtual time, "resume" unfreezes it, "step" runs `frames` frames and
    /// freezes again.
    pub action: TimeAction,
    /// Frames to run. Only read when `action` is "step".
    #[serde(default)]
    pub frames: Option<u32>,
}

#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimeAction {
    Pause,
    Resume,
    Step,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct GetLogsParams {
    /// Lowest level to return: TRACE, DEBUG, INFO, WARN or ERROR. Defaults to TRACE.
    #[serde(default)]
    pub min_level: Option<String>,
    /// Case-insensitive substring of the emitting module path, e.g. "example_game_lib".
    #[serde(default)]
    pub target_contains: Option<String>,
    /// Case-insensitive substring of the message.
    #[serde(default)]
    pub message_contains: Option<String>,
    /// Only entries newer than this sequence number, for polling without re-reading.
    #[serde(default)]
    pub after_sequence: Option<u64>,
    /// Defaults to 100. The newest matches are the ones returned.
    #[serde(default)]
    pub limit: Option<usize>,
}

// ---------------------------------------------------------------------------------------------

#[tool_router]
impl GameServer {
    #[tool(
        description = "Find entities in the running game by name substring, component, or parent. \
                       Returns each entity's id, name and absolute world position, named entities \
                       first. Start here, then world_list_components for what they carry.",
        meta = always_load()
    )]
    async fn world_query(&self, Parameters(params): Parameters<QueryParams>) -> ToolResult<Value> {
        let parent = match &params.parent {
            Some(selector) => Some(selector.resolve(&self.brp).await.map_err(fail)?.entity),
            None => None,
        };
        self.pass(
            "game.entities.list",
            &json!({
                "name_contains": params.name_contains,
                "with_components": params.with_components,
                "parent": parent,
                "include_internal": params.include_internal,
                "limit": params.limit,
                "fields": params.fields,
            }),
        )
        .await
    }

    #[tool(
        description = "Start here. The named entities as a tree, each with its position and a few \
                       tags such as Camera, DirectionalLight or RigidBody. Unnamed entities such as \
                       the window are not listed; find them with world_query by component. Use \
                       world_get_components to read values.",
        meta = always_load()
    )]
    async fn world_overview(
        &self,
        Parameters(params): Parameters<OverviewParams>,
    ) -> ToolResult<Value> {
        self.pass("game.entities.overview", &params).await
    }

    #[tool(
        description = "List the component types on one entity (`name` or `entity`) or several \
                       (`entities`). `not_reflected` ones can only be removed, not read or written."
    )]
    async fn world_list_components(
        &self,
        Parameters(params): Parameters<ListComponentsParams>,
    ) -> ToolResult<Value> {
        let single = params.selector.entity.is_some() || params.selector.name.is_some();
        let entities = match (single, params.entities.is_empty()) {
            (true, true) => vec![params.selector],
            (false, false) => params.entities,
            _ => {
                return Err(ErrorData::invalid_params(
                    "give one entity by `name` or `entity`, or several under `entities`.",
                    None,
                ));
            }
        };
        self.pass("game.components.list", &json!({ "entities": entities }))
            .await
    }

    #[tool(
        description = "Read component values from one entity, by `name` or `entity`. Types may be \
                       short names (\"Transform\"); \"Transform.rotation\" reads one field. \
                       Transform is relative to a grid cell; world_get_position gives the world \
                       position.",
        meta = always_load()
    )]
    async fn world_get_components(
        &self,
        Parameters(params): Parameters<GetComponentsParams>,
    ) -> ToolResult<Value> {
        if params.components.is_empty() {
            return Err(ErrorData::invalid_params(
                "name the component types to read. world_list_components lists what an entity \
                 has.",
                None,
            ));
        }
        self.pass("game.components.get", &params).await
    }

    #[tool(
        description = "Where an entity is: its absolute world position in metres, the grid that \
                       position is measured in, and the CellCoord, Transform and GlobalTransform \
                       it is computed from."
    )]
    async fn world_get_position(
        &self,
        Parameters(params): Parameters<EntityParams>,
    ) -> ToolResult<EntityPosition> {
        let mut request = serde_json::to_value(&params.selector).map_err(|err| fail(err.into()))?;
        request["components"] = json!([CELL_COORD, TRANSFORM, GLOBAL_TRANSFORM]);
        // A component the entity lacks is exactly the one whose field below is null.
        let mut values: ComponentValues = self
            .brp
            .call("game.components.get", request)
            .await
            .map_err(fail)?;

        #[derive(Deserialize)]
        struct Position {
            position: [f64; 3],
            grid: u64,
        }
        // An entity under no grid has no world position, which is an answer and not a failure:
        // its Transform still exists and is still what the caller came to see.
        let placed = self
            .brp
            .call::<Position>("game.position.get", json!({ "entity": values.entity }))
            .await
            .ok();

        self.tag(EntityPosition {
            identity: ResolvedEntity {
                entity: values.entity,
                name: values.name,
            },
            position: placed.as_ref().map(|p| p.position),
            grid: placed.map(|p| p.grid),
            cell: values.components.remove(CELL_COORD),
            transform: values.components.remove(TRANSFORM),
            global_transform: values.components.remove(GLOBAL_TRANSFORM),
            unreadable: values.unreadable,
        })
        .await
    }

    #[tool(
        description = "How to write a type: a one-line wire shape and an example value that the \
                       game accepts. Ask by short name or path before inserting or mutating a \
                       type for the first time.",
        meta = always_load()
    )]
    async fn registry_schema(
        &self,
        Parameters(params): Parameters<RegistrySchemaParams>,
    ) -> ToolResult<RegistrySchema> {
        let described: RegistrySchema = self
            .brp
            .call("game.registry.describe", json!(params))
            .await
            .map_err(fail)?;
        self.tag(described).await
    }

    #[tool(
        description = "Whether virtual time is running or frozen, how much of it has elapsed, \
                       and the frame number."
    )]
    async fn time_get(&self) -> ToolResult<Value> {
        let state = self
            .brp
            .call_raw("game.time.get", json!({}))
            .await
            .map_err(fail)?;
        self.tag(state).await
    }

    #[tool(
        description = "Freeze virtual time, resume it, or run a fixed number of frames and \
                       freeze again. Reading a component from a frozen world is the only way to \
                       compare two states."
    )]
    async fn time_set(&self, Parameters(params): Parameters<SetTimeParams>) -> ToolResult<Value> {
        let request = match params.action {
            TimeAction::Pause => json!({ "action": "pause" }),
            TimeAction::Resume => json!({ "action": "resume" }),
            TimeAction::Step => {
                let frames = params
                    .frames
                    .ok_or_else(|| ErrorData::invalid_params("`step` needs `frames`", None))?;
                json!({ "action": "step", "frames": frames })
            }
        };
        let started = self
            .brp
            .call_raw("game.time.set", request)
            .await
            .map_err(fail)?;

        if !matches!(params.action, TimeAction::Step) {
            return self.tag(started).await;
        }
        // A step is only useful if the caller can read the world after it, so the tool does not
        // return until the frames have run. The engine-side method cannot wait: it is itself a
        // system, running inside one of the frames being counted.
        let ended = self.await_step_end().await.map_err(fail)?;
        self.tag(ended).await
    }

    /// Polls until the world has repaused, or gives up after 10 seconds.
    ///
    /// The bound is against a game that is hung or stepping under a heavy frame time; without it
    /// a stalled game would hang the agent instead of telling it something is wrong.
    async fn await_step_end(&self) -> anyhow::Result<Value> {
        #[derive(Deserialize)]
        struct StepState {
            steps_remaining: Option<u32>,
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let state = self.brp.call_raw("game.time.get", json!({})).await?;
            let step: StepState = serde_json::from_value(state.clone())?;
            if step.steps_remaining.is_none() {
                return Ok(state);
            }
            if std::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "the step did not finish within 10 seconds; the game may be hung. Its state \
                     is {state}"
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    #[tool(
        description = "Read the game's recent log events, filtered by level, emitting module and \
                       message. The game's stderr is not otherwise visible.",
        meta = always_load()
    )]
    async fn log_get_entries(
        &self,
        Parameters(params): Parameters<GetLogsParams>,
    ) -> ToolResult<Value> {
        let entries = self
            .brp
            .call_raw(
                "game.logs.get",
                json!({
                    "min_level": params.min_level,
                    "target_contains": params.target_contains,
                    "message_contains": params.message_contains,
                    "after_sequence": params.after_sequence,
                    "limit": params.limit,
                }),
            )
            .await
            .map_err(fail)?;
        self.tag(entries).await
    }

    #[tool(
        description = "Create an entity with a name, components and an optional absolute world \
                       position. Types may be short names; values that needed fixing come back \
                       under `coerced`."
    )]
    async fn world_spawn_entity(
        &self,
        Parameters(params): Parameters<SpawnEntityParams>,
    ) -> ToolResult<Value> {
        self.pass("game.entities.spawn", &params).await
    }

    #[tool(description = "Delete an entity and everything parented to it.")]
    async fn world_despawn_entity(
        &self,
        Parameters(params): Parameters<EntityParams>,
    ) -> ToolResult<ResolvedEntity> {
        let identity = params.selector.resolve(&self.brp).await.map_err(fail)?;
        self.brp
            .call_raw("world.despawn_entity", json!({ "entity": identity.entity }))
            .await
            .map_err(fail)?;
        self.tag(identity).await
    }

    #[tool(
        description = "Add or replace whole components on an entity. A unit struct is {}, a \
                       newtype is its inner value."
    )]
    async fn world_insert_component(
        &self,
        Parameters(params): Parameters<InsertComponentParams>,
    ) -> ToolResult<Value> {
        self.pass("game.components.insert", &params).await
    }

    #[tool(
        description = "Remove components by short name or path. Works on components reflection \
                       cannot read."
    )]
    async fn world_remove_component(
        &self,
        Parameters(params): Parameters<RemoveComponentParams>,
    ) -> ToolResult<Value> {
        self.pass("game.components.remove", &params).await
    }

    #[tool(
        description = "Set one field (`path`, e.g. \"rotation\" or \"shadow_depth_bias\"; empty \
                       for the whole component) on one entity. Values use the shape \
                       registry_schema shows. Use world_set_position for position.",
        meta = always_load()
    )]
    async fn world_mutate_component(
        &self,
        Parameters(params): Parameters<MutateComponentParams>,
    ) -> ToolResult<Value> {
        self.pass("game.components.mutate", &params).await
    }

    #[tool(
        description = "Read resource values (global state such as debug settings), by short name \
                       or path; \"Type.field\" reads one field."
    )]
    async fn world_get_resources(
        &self,
        Parameters(params): Parameters<GetResourcesParams>,
    ) -> ToolResult<Value> {
        self.pass("game.resources.get", &params).await
    }

    #[tool(
        description = "Set one field of a resource (`path` empty for the whole resource). Values \
                       use the shape registry_schema shows."
    )]
    async fn world_mutate_resource(
        &self,
        Parameters(params): Parameters<MutateResourceParams>,
    ) -> ToolResult<Value> {
        self.pass("game.resources.mutate", &params).await
    }

    /// Sends the tool's params to a `game.*` method as they are and tags its answer. The game
    /// resolves names and types itself.
    async fn pass(&self, method: &str, params: &impl Serialize) -> ToolResult<Value> {
        let params = serde_json::to_value(params).map_err(|err| fail(err.into()))?;
        let answer = self.brp.call_raw(method, params).await.map_err(fail)?;
        self.tag(answer).await
    }

    #[tool(
        description = "Move an entity to a different parent, or detach it. Keeps the entity's \
                       components; its world position follows the new parent."
    )]
    async fn world_reparent_entity(
        &self,
        Parameters(params): Parameters<ReparentEntityParams>,
    ) -> ToolResult<ResolvedEntity> {
        let identity = params.selector.resolve(&self.brp).await.map_err(fail)?;
        let parent = match &params.parent {
            Some(selector) => Some(selector.resolve(&self.brp).await.map_err(fail)?.entity),
            None => None,
        };
        self.brp
            .call_raw(
                "world.reparent_entities",
                json!({ "entities": [identity.entity], "parent": parent }),
            )
            .await
            .map_err(fail)?;
        self.tag(identity).await
    }

    #[tool(
        description = "Move an entity to an absolute world position in metres. The only correct \
                       way to place something: a raw Transform is relative to a grid cell and a \
                       floating origin that both move.",
        meta = always_load()
    )]
    async fn world_set_position(
        &self,
        Parameters(params): Parameters<SetPositionParams>,
    ) -> ToolResult<PositionResult> {
        let identity = params.selector.resolve(&self.brp).await.map_err(fail)?;
        #[derive(Deserialize)]
        struct Position {
            position: [f64; 3],
        }
        let result: Position = self
            .brp
            .call(
                "game.position.set",
                json!({ "entity": identity.entity, "position": params.position }),
            )
            .await
            .map_err(fail)?;
        self.tag(PositionResult {
            identity,
            position: Some(result.position),
        })
        .await
    }
}

/// `_meta` asking Claude Code to load a tool upfront instead of deferring it behind tool search.
/// For the core tools only: each costs context every turn.
pub(crate) fn always_load() -> rmcp::model::MetaObject {
    let mut meta = rmcp::model::MetaObject::new();
    meta.insert("anthropic/alwaysLoad".into(), Value::Bool(true));
    meta
}

impl ServerHandler for GameServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Query and edit a running Bevy game. Entities are addressed by `name` or by `entity` \
             id; ids change every time the game restarts, names do not. Positions are absolute \
             metres with Y up. If a call reports that the game is unreachable, the game is not \
             running: game_status says why, game_start builds and starts it, game_restart \
             rebuilds it after a code change. Never start or kill the game by hand. Before you \
             finish, stop the games you started (game_status lists them under `yours`), unless \
             you are leaving one running for the user to inspect; then say so in your final \
             message. The game's own commands (screenshots, video, editor views, camera poses \
             and named viewpoints) appear as tools while it runs; command_list and command_call \
             reach them at any time. Pose a camera with camera_look_at or viewpoint_goto, not \
             world_set_position plus a quaternion. Start with world_overview. Types may be \
             short names. Before writing a type for the first time, read its example with \
             registry_schema. Values that needed fixing come back under `coerced`. If a result \
             says ename_mcp is out of date, do what it says and tell the user in your reply what \
             it asks of them, and why; if you may not build, ask the user to."
                .to_owned(),
        );
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_tool_list_changed()
            .build();
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        self.catalog.watch(&self.endpoint);
        if !self.catalog.is_loaded() {
            // One quick try, so a session that starts with the game running sees its commands
            // in the first listing rather than two seconds later.
            let brp = self.endpoint.snapshot();
            let _ =
                tokio::time::timeout(Duration::from_millis(500), self.catalog.refresh(&brp)).await;
        }
        let mut tools = self.tool_router.list_all();
        tools.extend(self.catalog.tools());
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        // The version-gated cache hints `#[tool_handler]` emits, but private: the list depends on
        // which game this one client's sidecar is talking to.
        let cache_hints = context
            .protocol_version()
            .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28);
        Ok(ListToolsResult {
            result_type: Some(ResultType::COMPLETE),
            tools,
            meta: None,
            next_cursor: None,
            ttl_ms: cache_hints.then_some(0),
            cache_scope: cache_hints.then_some(CacheScope::Private),
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.catalog.watch(&self.endpoint);
        let server = self.for_call();
        if server.tool_router.has_route(&request.name) {
            let call = ToolCallContext::new(&server, request, context);
            let response = match server.tool_router.call(call).await {
                Ok(response) => response,
                Err(error) => {
                    let mut content = vec![ContentBlock::text(error.message.into_owned())];
                    // A failure is where an error the game logged alongside matters most.
                    if let Some(notice) = server.failure_notice().await {
                        content.push(ContentBlock::text(notice));
                    }
                    CallToolResult::error(content).into()
                }
            };
            return Ok(warn_on_failure(response));
        }
        let name = request.name.to_string();
        let mut found = server.catalog.lookup(&name);
        if found.is_none() {
            server.catalog.refresh_and_notify(&server.brp).await;
            found = server.catalog.lookup(&name);
        }
        let Some((tool, run)) = found else {
            let mut message =
                format!("no tool `{name}`; the game may not be running, or may not register it");
            if let Some(warning) = staleness::failure_warning() {
                message += &format!("\n\n{warning}");
            }
            return Err(ErrorData::invalid_params(message, None));
        };
        let arguments = request.arguments.unwrap_or_default();
        Ok(warn_on_failure(
            server
                .call_command(&tool.command.name, arguments, Some(run))
                .await
                .into(),
        ))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_router
            .get(name)
            .cloned()
            .or_else(|| self.catalog.get(name).map(|tool| tool.tool))
    }

    /// Only a legacy client sends `notifications/initialized`; it hears about tool changes
    /// through its peer.
    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        self.catalog.set_legacy_peer(context.peer.clone());
        self.catalog.watch(&self.endpoint);
    }

    /// A 2026-07-28 client asks for changes with `subscriptions/listen`. Only tool changes are
    /// offered.
    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        let accepted = if requested.tools_list_changed == Some(true) {
            SubscriptionFilter::builder().tools_list_changed().build()
        } else {
            SubscriptionFilter::new()
        };
        Some(accepted)
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
        self.catalog.watch(&self.endpoint);
        if context.accepted().tools_list_changed != Some(true) {
            context.cancelled().await;
            return Ok(());
        }
        let id = self.catalog.add_subscription(context.sink().clone());
        context.cancelled().await;
        self.catalog.remove_subscription(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    /// The `index`th request and its body in; the response's `result` or `error` member out, or
    /// `None` to never answer, holding the connection open the way a hung game does.
    pub(super) type Responder = std::sync::Arc<
        dyn Fn(usize, &serde_json::Value) -> Option<serde_json::Value> + Send + Sync,
    >;

    pub(super) async fn serve(respond: Responder) -> String {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let served = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let served = served.clone();
                let respond = respond.clone();
                tokio::spawn(async move {
                    let mut buffer = Vec::new();
                    loop {
                        let mut chunk = [0; 4096];
                        let read = socket.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            return;
                        }
                        buffer.extend_from_slice(&chunk[..read]);
                        let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") else {
                            continue;
                        };
                        let head = String::from_utf8_lossy(&buffer[..end]).to_lowercase();
                        let length: usize = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .map_or(0, |value| value.trim().parse().unwrap());
                        if buffer.len() < end + 4 + length {
                            continue;
                        }
                        let request: serde_json::Value =
                            serde_json::from_slice(&buffer[end + 4..end + 4 + length]).unwrap();
                        buffer.drain(..end + 4 + length);

                        let index = served.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let Some(response) = respond(index, &request) else {
                            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                            return;
                        };
                        let mut body = json!({ "jsonrpc": "2.0", "id": request["id"] });
                        body.as_object_mut()
                            .unwrap()
                            .extend(response.as_object().unwrap().clone());
                        let body = body.to_string();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                             content-length: {}\r\n\r\n{body}",
                            body.len()
                        );
                        socket.write_all(response.as_bytes()).await.unwrap();
                    }
                });
            }
        });
        url
    }
}
