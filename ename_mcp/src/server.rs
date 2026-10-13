//! The tool catalogue.
//!
//! Design rules, in the order they matter:
//!
//! 1. Do not mirror BRP. A tool that is a one-to-one rename of a BRP method is a bad tool.
//! 2. Never reimplement reflection. Component values come from `world.get_components` and go
//!    back through `world.mutate_components`. No tool parses a property path itself.
//! 3. Anything settable is gettable.
//! 4. A tool returns data or it fails. There is no `{"success": false}` for the agent to
//!    remember to check.
//! 5. A tool's name and one sentence are all the model gets. Write them that way.
//!
//! Names are prefixed by group (`world_`, `time_`, `log_`, `editor_`, `screenshot_`) so a dispatching meta-tool could be
//! added later without renaming anything. At this size the indirection would only cost a round
//! trip.

use std::collections::HashMap;

use rmcp::{
    ErrorData, Json, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    brp::{BrpClient, BrpError, METHOD_NOT_FOUND},
    entity::{self, EntitySelector, ResolvedEntity},
    staleness,
};

/// The MCP server. One instance per client connection, one BRP client behind it.
#[derive(Clone)]
pub struct GameServer {
    brp: BrpClient,
    tool_router: ToolRouter<Self>,
}

impl GameServer {
    pub fn new(brp: BrpClient) -> Self {
        Self {
            brp,
            tool_router: Self::tool_router(),
        }
    }

    /// Attaches what the agent needs to know about this call to a finished tool result.
    ///
    /// The run id is read after the work rather than before, so it names the process that
    /// actually served the request. One extra loopback call per tool; the handler reads a
    /// single resource.
    async fn tag<T>(&self, result: T) -> ToolResult<T> {
        #[derive(Deserialize)]
        struct Pid {
            pid: String,
        }
        let pid: Pid = self
            .brp
            .call("game.pid.get", json!({}))
            .await
            .map_err(fail)?;
        Ok(Json(Tagged {
            pid: pid.pid,
            warning: staleness::warning(),
            result,
        }))
    }
}

/// Tool failures reach the agent as failures, with the sentence that explains them.
///
/// A stale build is appended here as well as carried on success, because that is the path it
/// actually shows up on: an old image talking to a newer game fails to parse the answer long
/// before it returns one.
fn fail(error: anyhow::Error) -> ErrorData {
    let message = match staleness::warning() {
        Some(warning) => format!("{error:#}\n\n{warning}"),
        None => format!("{error:#}"),
    };
    ErrorData::internal_error(message, None)
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
    /// Case-insensitive substrings of component type paths, e.g. "RigidBody". An entity must
    /// have a component matching every one of them.
    #[serde(default)]
    pub with_components: Vec<String>,
    /// Restricts the result to descendants of this entity.
    #[serde(default)]
    pub parent: Option<EntitySelector>,
    /// Include the entities Bevy uses to store resources and observers. Off by default: they
    /// are most of the world and none of them are scene content.
    #[serde(default)]
    pub include_internal: bool,
    /// Defaults to 100. A loaded glTF scene is hundreds of entities.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One entity as world_query reports it.
///
/// Typed rather than passed straight through, so the response has a fixed shape independent of
/// whatever `game.entities.list` happens to return.
#[derive(Serialize, schemars::JsonSchema)]
pub struct QueriedEntity {
    entity: u64,
    name: Option<String>,
    /// Absolute metres. Absent when the entity is not under a grid.
    position: Option<[f64; 3]>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct QueryResult {
    entities: Vec<QueriedEntity>,
    /// Entities that matched but were cut by `limit`. Zero means the list is complete.
    truncated: usize,
}

// ---------------------------------------------------------------------------------------------
// world_list_components

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListComponentsParams {
    /// The entities to list. Each is addressed by `name` or by `entity`, and several may be
    /// given in one call.
    pub entities: Vec<EntitySelector>,
}

/// One entity's component type paths.
#[derive(Serialize, schemars::JsonSchema)]
pub struct EntityComponentTypes {
    #[serde(flatten)]
    identity: ResolvedEntity,
    /// Full type paths, sorted. Pass the ones you want to world_get_components.
    components: Vec<String>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct ComponentTypes {
    entities: Vec<EntityComponentTypes>,
}

// ---------------------------------------------------------------------------------------------
// world_get_components and world_get_position

/// Which entity, and nothing else. For the tools whose whole input is the selector.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EntityParams {
    #[serde(flatten)]
    pub selector: EntitySelector,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetComponentsParams {
    #[serde(flatten)]
    pub selector: EntitySelector,
    /// Full component type paths, e.g. "bevy_transform::components::transform::Transform".
    /// world_list_components lists the paths an entity has; registry_schema turns a partial
    /// name into a full one.
    pub components: Vec<String>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct EntityComponents {
    #[serde(flatten)]
    identity: ResolvedEntity,
    /// Component type path to value.
    components: HashMap<String, Value>,
    /// Requested components the entity has but whose value could not be read, with the reason.
    /// Usually an asset handle, which reflection cannot serialize.
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    unreadable: HashMap<String, Value>,
    /// Requested paths the entity does not have. A misspelt path and a genuinely absent
    /// component look the same here; registry_schema says which it was.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    absent: Vec<String>,
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

/// What `world.get_components` answers with when `strict` is off.
#[derive(Default, Deserialize)]
#[serde(default)]
struct ComponentValues {
    components: HashMap<String, Value>,
    errors: HashMap<String, Value>,
}

// ---------------------------------------------------------------------------------------------
// registry_schema

/// Everything is optional, but a call with no filter at all returns whatever `limit` allows out
/// of more than a thousand types. Name the types you actually need.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct RegistrySchemaParams {
    /// Full type paths, e.g. "bevy_transform::components::transform::Transform". The precise
    /// way to ask, and the one to prefer.
    #[serde(default)]
    pub types: Vec<String>,
    /// Case-insensitive substring of the type path, for when the exact path is not known yet.
    #[serde(default)]
    pub contains: Option<String>,
    /// Only types from these crates, e.g. "avian3d" or "ename_engine".
    #[serde(default)]
    pub with_crates: Vec<String>,
    /// Exclude types from these crates.
    #[serde(default)]
    pub without_crates: Vec<String>,
    /// Defaults to 100. Schemas are large; the whole registry is roughly 775 KB.
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct RegistrySchema {
    /// Type path to its JSON schema, as the game's reflection registry describes it.
    schemas: HashMap<String, Value>,
    /// Types that matched but were cut by `limit`. Narrow the filters if this is not zero.
    truncated: usize,
    /// Paths in `types` that are not registered. An unregistered type is one the agent cannot
    /// read or write at all, so this is an answer rather than an oversight.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unregistered: Vec<String>,
}

// ---------------------------------------------------------------------------------------------
// mutation params

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SpawnEntityParams {
    /// The new entity's `Name`. Required: an unnamed entity can only ever be addressed by an id
    /// that expires when the game restarts.
    pub name: String,
    /// Component type path to value. Get the exact paths from registry_schema.
    #[serde(default)]
    pub components: HashMap<String, Value>,
    /// Absolute metres. Placing the entity also parents it to the world grid.
    #[serde(default)]
    pub position: Option<[f64; 3]>,
    /// Parent to attach to. Defaults to the world grid when `position` is given.
    #[serde(default)]
    pub parent: Option<EntitySelector>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct InsertComponentParams {
    #[serde(flatten)]
    pub selector: EntitySelector,
    /// Component type path to value. Get the exact paths from registry_schema.
    pub components: HashMap<String, Value>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RemoveComponentParams {
    #[serde(flatten)]
    pub selector: EntitySelector,
    /// Full component type paths.
    pub components: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MutateComponentParams {
    #[serde(flatten)]
    pub selector: EntitySelector,
    /// Full component type path, e.g. "bevy_transform::components::transform::Transform".
    pub component: String,
    /// Field path within the component, e.g. "scale.x". Empty replaces the whole component.
    pub path: String,
    /// The new value, shaped like the field's schema.
    pub value: Value,
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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetViewParams {
    /// "scene" is the editor's own free camera, "game" is the loaded stage's MainCamera.
    pub view: EditorView,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EditorView {
    Scene,
    Game,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScreenshotParams {
    /// Where to write the PNG. A relative path resolves against this MCP server's working
    /// directory, normally the project root. The result gives the absolute path written.
    pub path: String,
    /// "window" is the whole game window, editor panels included. "game_camera" is what the
    /// stage's MainCamera rendered (in the editor, the Game View). "editor_camera" is the
    /// editor's Scene View. A camera source needs its view to be the one shown; see
    /// editor_set_view.
    pub source: ScreenshotSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScreenshotSource {
    Window,
    GameCamera,
    EditorCamera,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SavedScreenshot {
    /// Absolute.
    path: String,
    /// Physical pixels.
    width: u32,
    height: u32,
    source: ScreenshotSource,
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
                       first. Start here, then world_list_components for what they carry."
    )]
    async fn world_query(
        &self,
        Parameters(params): Parameters<QueryParams>,
    ) -> ToolResult<QueryResult> {
        let parent = match &params.parent {
            Some(selector) => Some(selector.resolve(&self.brp).await.map_err(fail)?.entity),
            None => None,
        };
        let result: entity::ListResponse = self
            .brp
            .call(
                "game.entities.list",
                json!({
                    "name_contains": params.name_contains,
                    "with_components": params.with_components,
                    "parent": parent,
                    "include_internal": params.include_internal,
                    "limit": params.limit,
                }),
            )
            .await
            .map_err(fail)?;
        self.tag(QueryResult {
            entities: result
                .entities
                .into_iter()
                .map(|summary| QueriedEntity {
                    entity: summary.entity,
                    name: summary.name,
                    position: summary.position,
                })
                .collect(),
            truncated: result.truncated,
        })
        .await
    }

    #[tool(
        description = "List the component type paths on one or more entities. The step between \
                       world_query and world_get_components: it says what an entity has, without \
                       the cost of reading any values."
    )]
    async fn world_list_components(
        &self,
        Parameters(params): Parameters<ListComponentsParams>,
    ) -> ToolResult<ComponentTypes> {
        if params.entities.is_empty() {
            return Err(ErrorData::invalid_params(
                "name at least one entity, by `name` or by `entity`.",
                None,
            ));
        }

        let mut entities = Vec::with_capacity(params.entities.len());
        for selector in &params.entities {
            let identity = selector.resolve(&self.brp).await.map_err(fail)?;
            let components = self.list_components(identity.entity).await?;
            entities.push(EntityComponentTypes {
                identity,
                components,
            });
        }
        self.tag(ComponentTypes { entities }).await
    }

    #[tool(description = "Get specific components from an entity by ID or name.")]
    async fn world_get_components(
        &self,
        Parameters(params): Parameters<GetComponentsParams>,
    ) -> ToolResult<EntityComponents> {
        if params.components.is_empty() {
            return Err(ErrorData::invalid_params(
                "name the component type paths to read. world_list_components lists the paths an \
                 entity has, and registry_schema turns a partial name into a full one.",
                None,
            ));
        }
        let identity = params.selector.resolve(&self.brp).await.map_err(fail)?;
        let (values, absent) = self
            .read_components(identity.entity, &params.components)
            .await?;

        self.tag(EntityComponents {
            identity,
            components: values.components,
            unreadable: values.errors,
            absent,
        })
        .await
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
        let identity = params.selector.resolve(&self.brp).await.map_err(fail)?;

        let paths = [
            CELL_COORD.to_owned(),
            TRANSFORM.to_owned(),
            GLOBAL_TRANSFORM.to_owned(),
        ];
        // The absent ones need no report of their own here: a component the entity does not
        // have is exactly the one whose field below is null.
        let (mut values, _absent) = self.read_components(identity.entity, &paths).await?;

        #[derive(Deserialize)]
        struct Position {
            position: [f64; 3],
            grid: u64,
        }
        // An entity under no grid has no world position, which is an answer and not a failure:
        // its Transform still exists and is still what the caller came to see.
        let placed = self
            .brp
            .call::<Position>("game.position.get", json!({ "entity": identity.entity }))
            .await
            .ok();

        self.tag(EntityPosition {
            identity,
            position: placed.as_ref().map(|p| p.position),
            grid: placed.map(|p| p.grid),
            cell: values.components.remove(CELL_COORD),
            transform: values.components.remove(TRANSFORM),
            global_transform: values.components.remove(GLOBAL_TRANSFORM),
            unreadable: values.errors,
        })
        .await
    }

    /// The type paths of every component on one entity.
    ///
    /// BRP already sorts these, deliberately, so that no client comes to depend on archetype
    /// order. Sorting again costs nothing and means the guarantee is held on this side too.
    async fn list_components(&self, entity: u64) -> Result<Vec<String>, ErrorData> {
        let mut paths: Vec<String> = self
            .brp
            .call("world.list_components", json!({ "entity": entity }))
            .await
            .map_err(fail)?;
        paths.sort_unstable();
        Ok(paths)
    }

    /// Reads named component values, and says which of the names the entity does not have.
    ///
    /// The split is the point. `world.get_components` with `strict` off reports an absent
    /// component and one whose value will not serialize as the same kind of error, and those are
    /// different answers: the first says the entity is not what the caller thought, the second
    /// says reflection cannot show it. So membership is settled against `world.list_components`
    /// first, and only what the entity actually has is read.
    async fn read_components(
        &self,
        entity: u64,
        paths: &[String],
    ) -> Result<(ComponentValues, Vec<String>), ErrorData> {
        let on_entity = self.list_components(entity).await?;
        let (present, absent): (Vec<String>, Vec<String>) = paths
            .iter()
            .cloned()
            .partition(|path| on_entity.contains(path));

        if present.is_empty() {
            return Ok((ComponentValues::default(), absent));
        }
        let values = self
            .brp
            .call(
                "world.get_components",
                json!({ "entity": entity, "components": present, "strict": false }),
            )
            .await
            .map_err(fail)?;
        Ok((values, absent))
    }

    #[tool(
        description = "Get the JSON schema of registered types: the fields a component has, their \
                       names and their shapes. Read this before writing a component value. Name \
                       the types you want; the whole registry is over a thousand of them."
    )]
    async fn registry_schema(
        &self,
        Parameters(params): Parameters<RegistrySchemaParams>,
    ) -> ToolResult<RegistrySchema> {
        let limit = params.limit.unwrap_or(100);

        // The crate filters are the only ones BRP itself understands. Paths are matched here,
        // because `registry.schema` has no notion of a type path filter.
        let registry: HashMap<String, Value> = self
            .brp
            .call(
                "registry.schema",
                json!({
                    "with_crates": params.with_crates,
                    "without_crates": params.without_crates,
                }),
            )
            .await
            .map_err(fail)?;

        let unregistered: Vec<String> = params
            .types
            .iter()
            .filter(|path| !registry.contains_key(*path))
            .cloned()
            .collect();

        let needle = params.contains.as_ref().map(|c| c.to_lowercase());
        let mut matched: Vec<(String, Value)> = registry
            .into_iter()
            .filter(|(path, _)| params.types.is_empty() || params.types.contains(path))
            .filter(|(_, schema)| crate_matches(schema, &params.with_crates))
            .filter(|(path, _)| {
                needle
                    .as_ref()
                    .is_none_or(|n| path.to_lowercase().contains(n))
            })
            .collect();
        // Sorted so a truncated result is the same one every call, rather than whichever the
        // hash map happened to yield first.
        matched.sort_by(|a, b| a.0.cmp(&b.0));

        let truncated = matched.len().saturating_sub(limit);
        matched.truncate(limit);

        self.tag(RegistrySchema {
            schemas: matched.into_iter().collect(),
            truncated,
            unregistered,
        })
        .await
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
        description = "Which editor view is shown, Scene View (the editor's free camera) or Game \
                       View (the stage's MainCamera), and whether the Game View has a camera to \
                       show."
    )]
    async fn editor_get_view(&self) -> ToolResult<Value> {
        let view = self.call_editor_view("editor.view.get", json!({})).await?;
        self.tag(view).await
    }

    #[tool(
        description = "Show the editor's Scene View or Game View, as clicking its tab would. \
                       Returns after the game has run a few frames with it, so a screenshot taken \
                       next shows it."
    )]
    async fn editor_set_view(
        &self,
        Parameters(params): Parameters<SetViewParams>,
    ) -> ToolResult<Value> {
        self.set_view(params.view, VIEW_DEADLINE).await
    }

    /// `editor_set_view` under one deadline covering every request it makes, the readback and
    /// the run id included: the HTTP client has no timeout of its own, so a game that stops
    /// answering mid-call would otherwise hang the agent.
    async fn set_view(&self, view: EditorView, deadline: std::time::Duration) -> ToolResult<Value> {
        let work = async {
            self.call_editor_view("editor.view.set", json!({ "view": view }))
                .await?;
            self.await_frames(VIEW_SETTLE_FRAMES).await.map_err(fail)?;
            // Read back rather than echo the set: a click can still override it.
            let shown = self.call_editor_view("editor.view.get", json!({})).await?;
            self.tag(shown).await
        };
        tokio::time::timeout(deadline, work).await.map_err(|_| {
            fail(anyhow::anyhow!(
                "the game stopped answering: switching the view did not finish within {} \
                 seconds. It may be hung or stopped at a breakpoint.",
                deadline.as_secs_f32()
            ))
        })?
    }

    /// Calls an `editor.view.*` method, explaining a game build that does not have it.
    async fn call_editor_view(&self, method: &str, params: Value) -> Result<Value, ErrorData> {
        self.brp
            .call_raw(method, params)
            .await
            .map_err(explain_missing_editor_method)
            .map_err(fail)
    }

    /// Waits until the game has advanced `frames` frames from now. Unbounded: the caller sets
    /// the deadline.
    ///
    /// The frame counter is the main loop's, so this shows the loop ran, not that the GPU
    /// presented anything. It keeps counting while virtual time is paused.
    async fn await_frames(&self, frames: u32) -> anyhow::Result<()> {
        #[derive(Deserialize)]
        struct Frame {
            frame: u32,
        }
        let start: Frame = self.brp.call("game.time.get", json!({})).await?;
        loop {
            let now: Frame = self.brp.call("game.time.get", json!({})).await?;
            if now.frame.wrapping_sub(start.frame) >= frames {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[tool(
        description = "Capture the game window, the game camera or the editor camera to a PNG \
                       file. Returns the absolute path written and its size in pixels."
    )]
    async fn screenshot_capture(
        &self,
        Parameters(params): Parameters<ScreenshotParams>,
    ) -> ToolResult<SavedScreenshot> {
        let cwd = std::env::current_dir()
            .map_err(|err| fail(anyhow::anyhow!("cannot read the working directory: {err}")))?;
        self.capture_screenshot(params, &cwd, SCREENSHOT_DEADLINE)
            .await
    }

    /// `screenshot_capture` under one deadline covering every request, the run id included.
    async fn capture_screenshot(
        &self,
        ScreenshotParams { path, source }: ScreenshotParams,
        cwd: &std::path::Path,
        deadline: std::time::Duration,
    ) -> ToolResult<SavedScreenshot> {
        let path = absolute_png_path(&path, cwd)
            .map_err(|message| ErrorData::invalid_params(message, None))?;
        let work = async {
            let (method, params) = match source {
                ScreenshotSource::Window => (
                    "game.screenshot.capture",
                    json!({ "path": path, "source": "window" }),
                ),
                ScreenshotSource::GameCamera => (
                    "game.screenshot.capture",
                    json!({ "path": path, "source": "main_camera" }),
                ),
                ScreenshotSource::EditorCamera => {
                    ("editor.screenshot.capture", json!({ "path": path }))
                }
            };
            let explain = |error| fail(explain_screenshot_error(error, source));
            let started = self
                .brp
                .call_raw(method, params)
                .await
                .map_err(explain_missing_editor_method)
                .map_err(explain)?;
            let request = json!({ "request": started["request"] });

            #[derive(Deserialize)]
            #[serde(tag = "status", rename_all = "snake_case")]
            enum Status {
                Pending,
                Saved {
                    path: String,
                    width: u32,
                    height: u32,
                },
            }
            loop {
                let status: Status = self
                    .brp
                    .call("game.screenshot.result", request.clone())
                    .await
                    .map_err(explain)?;
                match status {
                    Status::Pending => {
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    }
                    Status::Saved {
                        path,
                        width,
                        height,
                    } => {
                        return self
                            .tag(SavedScreenshot {
                                path,
                                width,
                                height,
                                source,
                            })
                            .await;
                    }
                }
            }
        };
        tokio::time::timeout(deadline, work).await.map_err(|_| {
            fail(anyhow::anyhow!(
                "the game stopped answering: the screenshot did not finish within {} seconds. It \
                 may be hung, stopped at a breakpoint, or minimized.",
                deadline.as_secs_f32()
            ))
        })?
    }

    #[tool(
        description = "Read the game's recent log events, filtered by level, emitting module and \
                       message. The game's stderr is not otherwise visible."
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
        description = "Create an entity with a name, a set of components and an optional absolute \
                       world position."
    )]
    async fn world_spawn_entity(
        &self,
        Parameters(params): Parameters<SpawnEntityParams>,
    ) -> ToolResult<PositionResult> {
        let mut components = params.components;
        components.insert("bevy_ecs::name::Name".to_owned(), json!(params.name));

        #[derive(Deserialize)]
        struct Spawned {
            entity: u64,
        }
        let spawned: Spawned = self
            .brp
            .call("world.spawn_entity", json!({ "components": components }))
            .await
            .map_err(fail)?;

        // Placement is two steps because an entity's position is only meaningful relative to the
        // grid it hangs under, and `world.spawn_entity` cannot set a parent.
        let parent = match (&params.parent, params.position) {
            (Some(selector), _) => Some(selector.resolve(&self.brp).await.map_err(fail)?.entity),
            (None, Some(_)) => Some(self.world_grid().await.map_err(fail)?),
            (None, None) => None,
        };
        if let Some(parent) = parent {
            self.brp
                .call_raw(
                    "world.reparent_entities",
                    json!({ "entities": [spawned.entity], "parent": parent }),
                )
                .await
                .map_err(fail)?;
        }
        if let Some(position) = params.position {
            self.brp
                .call_raw(
                    "game.position.set",
                    json!({ "entity": spawned.entity, "position": position }),
                )
                .await
                .map_err(fail)?;
        }

        let identity = ResolvedEntity {
            entity: spawned.entity,
            name: Some(params.name),
        };
        self.tag(PositionResult {
            identity,
            position: params.position,
        })
        .await
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

    #[tool(description = "Add components to an existing entity, or replace them if present.")]
    async fn world_insert_component(
        &self,
        Parameters(params): Parameters<InsertComponentParams>,
    ) -> ToolResult<ResolvedEntity> {
        let identity = params.selector.resolve(&self.brp).await.map_err(fail)?;
        self.brp
            .call_raw(
                "world.insert_components",
                json!({ "entity": identity.entity, "components": params.components }),
            )
            .await
            .map_err(fail)?;
        self.tag(identity).await
    }

    #[tool(description = "Remove components from an entity by their full type paths.")]
    async fn world_remove_component(
        &self,
        Parameters(params): Parameters<RemoveComponentParams>,
    ) -> ToolResult<ResolvedEntity> {
        let identity = params.selector.resolve(&self.brp).await.map_err(fail)?;
        self.brp
            .call_raw(
                "world.remove_components",
                json!({ "entity": identity.entity, "components": params.components }),
            )
            .await
            .map_err(fail)?;
        self.tag(identity).await
    }

    #[tool(
        description = "Set one field of one component on an entity, addressed by field path. The \
                       main write tool. Use world_set_position for position."
    )]
    async fn world_mutate_component(
        &self,
        Parameters(params): Parameters<MutateComponentParams>,
    ) -> ToolResult<ResolvedEntity> {
        reject_position_write(&params.component, &params.path)?;
        let identity = params.selector.resolve(&self.brp).await.map_err(fail)?;
        self.brp
            .call_raw(
                "world.mutate_components",
                json!({
                    "entity": identity.entity,
                    "component": params.component,
                    "path": params.path,
                    "value": params.value,
                }),
            )
            .await
            .map_err(fail)?;
        self.tag(identity).await
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
                       floating origin that both move."
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

    /// The entity holding the world's root `Grid`, which is what a placed entity hangs under.
    async fn world_grid(&self) -> anyhow::Result<u64> {
        let response: entity::ListResponse = self
            .brp
            .call(
                "game.entities.list",
                json!({ "with_components": ["big_space::grid::Grid"], "limit": 100 }),
            )
            .await?;

        match response.entities.as_slice() {
            [] => anyhow::bail!("the game has no big_space grid, so nothing can be placed in it"),
            [only] => Ok(only.entity),
            many => anyhow::bail!(
                "the game has {} grids ({}). Pass `parent` to say which one.",
                many.len(),
                many.iter()
                    .map(|g| format!("{} `{}`", g.entity, g.name.as_deref().unwrap_or("")))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// Whether a schema belongs to one of `crates`, with an empty list meaning "any".
///
/// `registry.schema` applies its own `with_crates` only to types that have a crate name, so
/// `&str`, `()` and tuple types come back whatever is asked for. Repeating the test here is what
/// makes the filter mean what its name says.
fn crate_matches(schema: &Value, crates: &[String]) -> bool {
    if crates.is_empty() {
        return true;
    }
    schema
        .get("crateName")
        .or_else(|| schema.get("crate_name"))
        .and_then(Value::as_str)
        .is_some_and(|name| crates.iter().any(|wanted| wanted == name))
}

/// Refuses a mutation that would write half of a position.
///
/// `Transform.translation` is an offset within one grid cell relative to a floating origin that
/// moves as the camera moves, so writing it sets a different world point every time. Failing
/// here is the whole reason `world_set_position` exists.
fn reject_position_write(component: &str, path: &str) -> Result<(), ErrorData> {
    let touches_translation =
        component.ends_with("::Transform") && (path.is_empty() || path.starts_with("translation"));
    let touches_cell = component.ends_with("::CellCoord");
    if touches_translation || touches_cell {
        return Err(ErrorData::invalid_params(
            "position is a grid cell plus an offset from a moving origin, so writing either half \
             alone lands somewhere else. Use world_set_position with absolute metres.",
            None,
        ));
    }
    Ok(())
}

/// Frames `editor_set_view` waits for: one for the egui pass that applies the switch, one for
/// the render world, which runs a frame behind under pipelined rendering, and one to spare.
const VIEW_SETTLE_FRAMES: u32 = 3;

/// How long `editor_set_view` may take end to end.
const VIEW_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// How long `screenshot_capture` may take end to end. The engine gives up on a readback after
/// 5 s, so this leaves room for the write.
const SCREENSHOT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Rewords "method not found" from an `editor.*` call. It means a game binary without the
/// editor, or one built before the editor had these methods, never that the game is down.
fn explain_missing_editor_method(error: anyhow::Error) -> anyhow::Error {
    match error.downcast_ref::<BrpError>() {
        Some(brp) if brp.code == METHOD_NOT_FOUND && brp.method.starts_with("editor.") => {
            let feature = if brp.method.starts_with("editor.screenshot.") {
                "editor camera screenshots"
            } else {
                "editor view switching"
            };
            anyhow::anyhow!(
                "this game build does not expose {feature}. Run the editor binary, rebuilt \
                 against the current ename_editor."
            )
        }
        _ => error,
    }
}

/// Makes `path` absolute against `cwd`, the sidecar's working directory: the game's is not the
/// agent's. Refuses anything that is not a `.png`.
fn absolute_png_path(path: &str, cwd: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let path = std::path::Path::new(path);
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
    {
        return Err(format!(
            "`{}` must end in .png; screenshots are only written as PNG",
            path.display()
        ));
    }
    Ok(cwd.join(path))
}

/// Points an inactive-camera failure at the view switch that fixes it.
fn explain_screenshot_error(error: anyhow::Error, source: ScreenshotSource) -> anyhow::Error {
    let view = match source {
        ScreenshotSource::GameCamera => "game",
        ScreenshotSource::EditorCamera => "scene",
        ScreenshotSource::Window => return error,
    };
    if format!("{error:#}").contains("is not active") {
        anyhow::anyhow!(
            "{error:#}. That camera only renders while its view is shown: call editor_set_view \
             with view={view} first."
        )
    } else {
        error
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for GameServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Query and edit a running Bevy game. Entities are addressed by `name` or by `entity` \
             id; ids change every time the game restarts, names do not. Positions are absolute \
             metres with Y up. If a call reports that the game is unreachable, the game is not \
             running."
                .to_owned(),
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        CELL_COORD as CELL, TRANSFORM, crate_matches, explain_missing_editor_method,
        reject_position_write,
    };
    use super::{EditorView, GameServer, ScreenshotParams, ScreenshotSource};
    use crate::brp::{BrpClient, BrpError, METHOD_NOT_FOUND};
    use std::time::Duration;

    fn brp_error(method: &str, code: i32) -> anyhow::Error {
        BrpError {
            method: method.to_owned(),
            code,
            message: "Method not found".to_owned(),
        }
        .into()
    }

    /// A BRP server that answers `editor_set_view`'s requests in order and never answers
    /// request number `stall_at`, holding the connection open the way a hung game does.
    async fn fake_game(stall_at: Option<usize>) -> String {
        serve(std::sync::Arc::new(
            move |index, request: &serde_json::Value| {
                if Some(index) == stall_at {
                    return None;
                }
                let result = match request["method"].as_str().unwrap() {
                    "game.time.get" => json!({ "frame": index * 3 }),
                    "game.pid.get" => json!({ "pid": "abc" }),
                    _ => json!({ "view": "game", "game_camera": true }),
                };
                Some(json!({ "result": result }))
            },
        ))
        .await
    }

    /// The `index`th request and its body in; the response's `result` or `error` member out, or
    /// `None` to never answer, holding the connection open the way a hung game does.
    type Responder = std::sync::Arc<
        dyn Fn(usize, &serde_json::Value) -> Option<serde_json::Value> + Send + Sync,
    >;

    async fn serve(respond: Responder) -> String {
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

    /// A game that answers screenshot calls: `capture` with request 7, `result` pending once
    /// then `saved`, unless `fail` gives an error for the result. Records each method and its
    /// params.
    async fn screenshot_game(
        fail: Option<&'static str>,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>,
    ) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        let polls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let url = serve(std::sync::Arc::new(
            move |_, request: &serde_json::Value| {
                let method = request["method"].as_str().unwrap().to_owned();
                log.lock()
                    .unwrap()
                    .push((method.clone(), request["params"].clone()));
                Some(match method.as_str() {
                    "game.pid.get" => json!({ "result": { "pid": "abc" } }),
                    "game.screenshot.result" => match fail {
                        Some(message) => json!({ "error": { "code": -32603, "message": message } }),
                        None if polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 => {
                            json!({ "result": { "status": "pending" } })
                        }
                        None => json!({ "result": {
                        "status": "saved",
                        "path": request["params"]["request"].to_string(),
                        "width": 4,
                        "height": 3,
                    } }),
                    },
                    _ => json!({ "result": { "request": 7 } }),
                })
            },
        ))
        .await;
        (url, seen)
    }

    fn expect_err<T>(result: Result<T, rmcp::ErrorData>) -> rmcp::ErrorData {
        match result {
            Ok(_) => panic!("the call succeeded"),
            Err(error) => error,
        }
    }

    fn screenshot(path: &str, source: ScreenshotSource) -> ScreenshotParams {
        ScreenshotParams {
            path: path.to_owned(),
            source,
        }
    }

    #[tokio::test]
    async fn screenshot_routes_each_source_and_polls_until_saved() {
        let cwd = std::path::Path::new("/project");
        for (source, method, params) in [
            (
                ScreenshotSource::Window,
                "game.screenshot.capture",
                json!({ "path": "/project/shots/a.png", "source": "window" }),
            ),
            (
                ScreenshotSource::GameCamera,
                "game.screenshot.capture",
                json!({ "path": "/project/shots/a.png", "source": "main_camera" }),
            ),
            (
                ScreenshotSource::EditorCamera,
                "editor.screenshot.capture",
                json!({ "path": "/project/shots/a.png" }),
            ),
        ] {
            let (url, seen) = screenshot_game(None).await;
            let server = GameServer::new(BrpClient::new(url));
            let result = server
                .capture_screenshot(
                    screenshot("shots/a.png", source),
                    cwd,
                    Duration::from_secs(5),
                )
                .await
                .unwrap();
            assert_eq!(
                serde_json::to_value(result.0).unwrap(),
                json!({ "pid": "abc", "path": "7", "width": 4, "height": 3, "source": source }),
            );
            let seen = seen.lock().unwrap();
            assert_eq!(seen[0], (method.to_owned(), params), "{source:?}");
            let polls = seen
                .iter()
                .filter(|(method, _)| method == "game.screenshot.result")
                .collect::<Vec<_>>();
            assert_eq!(polls.len(), 2, "{source:?}");
            assert_eq!(polls[0].1, json!({ "request": 7 }));
        }
    }

    #[tokio::test]
    async fn screenshot_keeps_an_absolute_path_and_refuses_other_formats() {
        let (url, seen) = screenshot_game(None).await;
        let server = GameServer::new(BrpClient::new(url));
        let cwd = std::path::Path::new("/project");
        server
            .capture_screenshot(
                screenshot("/elsewhere/a.PNG", ScreenshotSource::Window),
                cwd,
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(seen.lock().unwrap()[0].1["path"], "/elsewhere/a.PNG");

        let (url, seen) = screenshot_game(None).await;
        let server = GameServer::new(BrpClient::new(url));
        let error = expect_err(
            server
                .capture_screenshot(
                    screenshot("a.jpg", ScreenshotSource::Window),
                    cwd,
                    Duration::from_secs(5),
                )
                .await,
        );
        assert!(error.message.contains(".png"), "{}", error.message);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_inactive_camera_says_which_view_to_show() {
        for (source, hint) in [
            (ScreenshotSource::GameCamera, "view=game"),
            (ScreenshotSource::EditorCamera, "view=scene"),
        ] {
            let (url, _) = screenshot_game(Some("camera 5v1 is not active this frame")).await;
            let server = GameServer::new(BrpClient::new(url));
            let error = expect_err(
                server
                    .capture_screenshot(
                        screenshot("/a.png", source),
                        std::path::Path::new("/"),
                        Duration::from_secs(5),
                    )
                    .await,
            );
            assert!(error.message.contains("is not active"), "{}", error.message);
            assert!(error.message.contains(hint), "{}", error.message);
        }
    }

    #[tokio::test]
    async fn screenshot_gives_up_when_the_game_stops_answering() {
        let server = GameServer::new(BrpClient::new(fake_game(Some(0)).await));
        let error = expect_err(
            server
                .capture_screenshot(
                    screenshot("/a.png", ScreenshotSource::Window),
                    std::path::Path::new("/"),
                    Duration::from_millis(300),
                )
                .await,
        );
        assert!(
            error.message.contains("stopped answering"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_missing_editor_screenshot_method_says_the_build_lacks_it() {
        let error =
            explain_missing_editor_method(brp_error("editor.screenshot.capture", METHOD_NOT_FOUND));
        assert!(
            error
                .to_string()
                .contains("does not expose editor camera screenshots")
        );
    }

    /// set, two frame reads, readback, run id.
    const SET_VIEW_REQUESTS: usize = 5;

    #[tokio::test]
    async fn set_view_reads_the_view_back() {
        let server = GameServer::new(BrpClient::new(fake_game(None).await));
        let result = server
            .set_view(EditorView::Game, Duration::from_secs(5))
            .await
            .unwrap();
        let value = serde_json::to_value(result.0).unwrap();
        assert_eq!(
            value,
            json!({ "pid": "abc", "view": "game", "game_camera": true })
        );
    }

    #[tokio::test]
    async fn set_view_gives_up_wherever_the_game_stops_answering() {
        for stall_at in 0..SET_VIEW_REQUESTS {
            let server = GameServer::new(BrpClient::new(fake_game(Some(stall_at)).await));
            let Err(error) = server
                .set_view(EditorView::Game, Duration::from_millis(300))
                .await
            else {
                panic!("stalled at request {stall_at}, yet the call succeeded");
            };
            assert!(
                error.message.contains("stopped answering"),
                "stalled at request {stall_at}: {}",
                error.message
            );
        }
    }

    #[test]
    fn a_missing_editor_method_says_the_build_lacks_it() {
        let error = explain_missing_editor_method(brp_error("editor.view.set", METHOD_NOT_FOUND));
        assert!(
            error
                .to_string()
                .contains("does not expose editor view switching")
        );
    }

    #[test]
    fn other_errors_pass_through_unchanged() {
        for (method, code) in [
            ("editor.view.set", -23402),
            ("game.time.get", METHOD_NOT_FOUND),
        ] {
            let error = explain_missing_editor_method(brp_error(method, code));
            assert!(
                error.downcast_ref::<BrpError>().is_some(),
                "{method} {code}"
            );
        }
    }

    #[test]
    fn refuses_writes_that_would_move_half_a_position() {
        for (component, path) in [
            (TRANSFORM, "translation"),
            (TRANSFORM, "translation.x"),
            (TRANSFORM, ""),
            (CELL, "x"),
        ] {
            assert!(
                reject_position_write(component, path).is_err(),
                "`{component}` `{path}` should be refused"
            );
        }
    }

    #[test]
    fn allows_writes_to_the_rest_of_a_transform() {
        for path in ["scale", "scale.x", "rotation"] {
            assert!(reject_position_write(TRANSFORM, path).is_ok(), "`{path}`");
        }
    }

    /// `translation` is a prefix of nothing else on `Transform` today, but the check is a string
    /// comparison and a future field starting with those letters must not be caught by it.
    #[test]
    fn does_not_refuse_an_unrelated_component() {
        assert!(reject_position_write("some::other::Type", "translation").is_ok());
    }

    #[test]
    fn an_empty_crate_filter_matches_everything() {
        assert!(crate_matches(&json!({"crateName": "avian3d"}), &[]));
        assert!(crate_matches(&json!({}), &[]));
    }

    /// The case upstream gets wrong: `&str` and `()` have no crate name and `registry.schema`
    /// lets them through whatever `with_crates` says.
    #[test]
    fn a_crate_filter_rejects_types_with_no_crate() {
        let wanted = ["ename_engine".to_owned()];
        assert!(!crate_matches(&json!({}), &wanted));
        assert!(!crate_matches(
            &json!({"crateName": "bevy_transform"}),
            &wanted
        ));
        assert!(crate_matches(
            &json!({"crateName": "ename_engine"}),
            &wanted
        ));
    }
}
