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
//! Names are prefixed by group (`world_`, `run_`, `log_`) so a dispatching meta-tool could be
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
    brp::BrpClient,
    entity::{EntitySelector, ResolvedEntity},
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

    /// Attaches the game's run id to a finished tool result.
    ///
    /// Read after the work rather than before, so it names the process that actually served the
    /// request. One extra loopback call per tool; the handler reads a single resource.
    async fn tagged<T>(&self, result: T) -> ToolResult<T> {
        #[derive(Deserialize)]
        struct Pid {
            pid: String,
        }
        let pid: Pid = self
            .brp
            .call("game.pid.get", json!({}))
            .await
            .map_err(fail)?;
        Ok(Json(WithPid {
            pid: pid.pid,
            result,
        }))
    }
}

/// Tool failures reach the agent as failures, with the sentence that explains them.
fn fail(error: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(format!("{error:#}"), None)
}

type ToolResult<T> = Result<Json<WithPid<T>>, ErrorData>;

/// Every tool result, wrapped with the id of the game process that answered it.
///
/// Nothing the agent holds between calls survives a restart, entity ids least of all. Without
/// this the agent finds out by watching a plan fail against a world it never saw. Three letters
/// so the cost of carrying it on every response stays negligible.
#[derive(Serialize, schemars::JsonSchema)]
pub struct WithPid<T> {
    /// Identifies this run of the game process. A different value from the last call means the
    /// game restarted: re-resolve entities by name, because the ids are stale.
    pid: String,
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
    /// world_query lists the paths an entity has; registry_schema turns a partial name into a
    /// full one.
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
    position: [f64; 3],
}

// ---------------------------------------------------------------------------------------------
// run control and logs

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetRunStateParams {
    /// "pause" freezes virtual time, "resume" unfreezes it, "step" runs `frames` frames and
    /// freezes again.
    pub action: RunAction,
    /// Frames to run. Only read when `action` is "step".
    #[serde(default)]
    pub frames: Option<u32>,
}

#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunAction {
    Pause,
    Resume,
    Step,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct GetLogsParams {
    /// Lowest level to return: TRACE, DEBUG, INFO, WARN or ERROR. Defaults to TRACE.
    #[serde(default)]
    pub min_level: Option<String>,
    /// Case-insensitive substring of the emitting module path, e.g. "ename_game".
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
                       Returns each entity's id, name, component type paths and absolute world \
                       position, named entities first. Start here."
    )]
    async fn world_query(&self, Parameters(params): Parameters<QueryParams>) -> ToolResult<Value> {
        let parent = match &params.parent {
            Some(selector) => Some(selector.resolve(&self.brp).await.map_err(fail)?.entity),
            None => None,
        };
        let result = self
            .brp
            .call_raw(
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
        self.tagged(result).await
    }

    #[tool(description = "Get specific components from an entity by ID or name.")]
    async fn world_get_components(
        &self,
        Parameters(params): Parameters<GetComponentsParams>,
    ) -> ToolResult<EntityComponents> {
        if params.components.is_empty() {
            return Err(ErrorData::invalid_params(
                "name the component type paths to read. world_query lists the paths an entity \
                 has, and registry_schema turns a partial name into a full one.",
                None,
            ));
        }
        let identity = params.selector.resolve(&self.brp).await.map_err(fail)?;
        let (values, absent) = self
            .read_components(identity.entity, &params.components)
            .await?;

        self.tagged(EntityComponents {
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

        self.tagged(EntityPosition {
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
        let on_entity: Vec<String> = self
            .brp
            .call("world.list_components", json!({ "entity": entity }))
            .await
            .map_err(fail)?;
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

        self.tagged(RegistrySchema {
            schemas: matched.into_iter().collect(),
            truncated,
            unregistered,
        })
        .await
    }

    #[tool(
        description = "Whether the world is running or frozen, how much virtual time has elapsed, \
                       the frame number, and the current game state."
    )]
    async fn run_get_state(&self) -> ToolResult<Value> {
        let state = self
            .brp
            .call_raw("game.run_state.get", json!({}))
            .await
            .map_err(fail)?;
        self.tagged(state).await
    }

    #[tool(
        description = "Freeze the world, resume it, or run a fixed number of frames and freeze \
                       again. Reading a component from a frozen world is the only way to compare \
                       two states."
    )]
    async fn run_set_state(
        &self,
        Parameters(params): Parameters<SetRunStateParams>,
    ) -> ToolResult<Value> {
        let request = match params.action {
            RunAction::Pause => json!({ "action": "pause" }),
            RunAction::Resume => json!({ "action": "resume" }),
            RunAction::Step => {
                let frames = params
                    .frames
                    .ok_or_else(|| ErrorData::invalid_params("`step` needs `frames`", None))?;
                json!({ "action": "step", "frames": frames })
            }
        };
        let started = self
            .brp
            .call_raw("game.run_state.set", request)
            .await
            .map_err(fail)?;

        if !matches!(params.action, RunAction::Step) {
            return self.tagged(started).await;
        }
        // A step is only useful if the caller can read the world after it, so the tool does not
        // return until the frames have run. The engine-side method cannot wait: it is itself a
        // system, running inside one of the frames being counted.
        let ended = self.await_step_end().await.map_err(fail)?;
        self.tagged(ended).await
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
            let state = self.brp.call_raw("game.run_state.get", json!({})).await?;
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
        self.tagged(entries).await
    }

    #[tool(
        description = "Create an entity with a name, a set of components and an optional absolute \
                       world position."
    )]
    async fn world_spawn_entity(
        &self,
        Parameters(params): Parameters<SpawnEntityParams>,
    ) -> ToolResult<Value> {
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

        self.tagged(json!({
            "entity": spawned.entity,
            "name": params.name,
            "position": params.position,
        }))
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
        self.tagged(identity).await
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
        self.tagged(identity).await
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
        self.tagged(identity).await
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
        self.tagged(identity).await
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
        self.tagged(identity).await
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
        self.tagged(PositionResult {
            identity,
            position: result.position,
        })
        .await
    }

    /// The entity holding the world's root `Grid`, which is what a placed entity hangs under.
    async fn world_grid(&self) -> anyhow::Result<u64> {
        #[derive(Deserialize)]
        struct Summary {
            entity: u64,
            name: Option<String>,
        }
        #[derive(Deserialize)]
        struct ListResponse {
            entities: Vec<Summary>,
        }
        let response: ListResponse = self
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

    use super::{CELL_COORD as CELL, TRANSFORM, crate_matches, reject_position_write};

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
