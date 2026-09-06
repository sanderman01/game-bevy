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
}

/// Tool failures reach the agent as failures, with the sentence that explains them.
fn fail(error: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(format!("{error:#}"), None)
}

type ToolResult<T> = Result<Json<T>, ErrorData>;

// ---------------------------------------------------------------------------------------------
// world_list_entities

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct ListEntitiesParams {
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
    /// Defaults to 100. A loaded glTF scene is hundreds of entities.
    #[serde(default)]
    pub limit: Option<usize>,
}

// ---------------------------------------------------------------------------------------------
// world_get_entity

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetEntityParams {
    #[serde(flatten)]
    pub selector: EntitySelector,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct EntityDetail {
    #[serde(flatten)]
    identity: ResolvedEntity,
    /// Absolute metres. Absent when the entity is not placed in the world.
    position: Option<[f64; 3]>,
    /// Component type path to value.
    components: HashMap<String, Value>,
    /// Components present on the entity whose value could not be read, with the reason.
    /// Usually an asset handle, which reflection cannot serialize.
    unreadable: HashMap<String, Value>,
}

// ---------------------------------------------------------------------------------------------
// world_list_component_types

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListComponentTypesParams {
    /// Case-insensitive substring of the type path, e.g. "transform" or "rigidbody".
    pub contains: String,
    /// Defaults to 25. The full registry is over a thousand types.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Include each type's field schema. Off by default because the schemas are large.
    #[serde(default)]
    pub with_schema: bool,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct ComponentType {
    type_path: String,
    short_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    schema: Option<Value>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct ComponentTypes {
    types: Vec<ComponentType>,
    /// Types that matched but were cut by `limit`. Narrow `contains` if this is not zero.
    truncated: usize,
}

// ---------------------------------------------------------------------------------------------
// mutation params

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SpawnEntityParams {
    /// The new entity's `Name`. Required: an unnamed entity can only ever be addressed by an id
    /// that expires when the game restarts.
    pub name: String,
    /// Component type path to value. Get the exact paths from world_list_component_types.
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
    /// Component type path to value. Get the exact paths from world_list_component_types.
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
                       position. Start here."
    )]
    async fn world_list_entities(
        &self,
        Parameters(params): Parameters<ListEntitiesParams>,
    ) -> ToolResult<Value> {
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
                    "limit": params.limit,
                }),
            )
            .await
            .map_err(fail)?;
        Ok(Json(result))
    }

    #[tool(
        description = "Read every component on one entity, with its value and its absolute world \
                       position. Address the entity by name or by id."
    )]
    async fn world_get_entity(
        &self,
        Parameters(params): Parameters<GetEntityParams>,
    ) -> ToolResult<EntityDetail> {
        let identity = params.selector.resolve(&self.brp).await.map_err(fail)?;

        let types: Vec<String> = self
            .brp
            .call(
                "world.list_components",
                json!({ "entity": identity.entity }),
            )
            .await
            .map_err(fail)?;

        #[derive(Deserialize)]
        struct Components {
            components: HashMap<String, Value>,
            errors: HashMap<String, Value>,
        }
        let values: Components = self
            .brp
            .call(
                "world.get_components",
                json!({ "entity": identity.entity, "components": types, "strict": false }),
            )
            .await
            .map_err(fail)?;

        #[derive(Deserialize)]
        struct Position {
            position: [f64; 3],
        }
        let position = self
            .brp
            .call::<Position>("game.position.get", json!({ "entity": identity.entity }))
            .await
            .ok()
            .map(|p| p.position);

        Ok(Json(EntityDetail {
            identity,
            position,
            components: values.components,
            unreadable: values.errors,
        }))
    }

    #[tool(
        description = "Look up the fully-qualified Rust type paths of components matching a \
                       substring. Every write tool needs the exact path, which is longer than \
                       anything worth guessing."
    )]
    async fn world_list_component_types(
        &self,
        Parameters(params): Parameters<ListComponentTypesParams>,
    ) -> ToolResult<ComponentTypes> {
        let limit = params.limit.unwrap_or(25);
        let needle = params.contains.to_lowercase();

        let schema: HashMap<String, Value> = self
            .brp
            .call("registry.schema", json!({}))
            .await
            .map_err(fail)?;

        let mut matches: Vec<ComponentType> = schema
            .into_iter()
            .filter(|(path, _)| path.to_lowercase().contains(&needle))
            .map(|(type_path, value)| ComponentType {
                short_path: value
                    .get("shortPath")
                    .or_else(|| value.get("short_path"))
                    .and_then(Value::as_str)
                    .unwrap_or(&type_path)
                    .to_owned(),
                schema: params.with_schema.then_some(value),
                type_path,
            })
            .collect();
        matches.sort_by(|a, b| a.type_path.cmp(&b.type_path));

        let truncated = matches.len().saturating_sub(limit);
        matches.truncate(limit);
        Ok(Json(ComponentTypes {
            types: matches,
            truncated,
        }))
    }

    #[tool(
        description = "Whether the world is running or frozen, how much virtual time has elapsed, \
                       the frame number, and the current game state."
    )]
    async fn run_get_state(&self) -> ToolResult<Value> {
        Ok(Json(
            self.brp
                .call_raw("game.run_state.get", json!({}))
                .await
                .map_err(fail)?,
        ))
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
            return Ok(Json(started));
        }
        // A step is only useful if the caller can read the world after it, so the tool does not
        // return until the frames have run. The engine-side method cannot wait: it is itself a
        // system, running inside one of the frames being counted.
        self.await_step_end().await.map(Json).map_err(fail)
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
        Ok(Json(
            self.brp
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
                .map_err(fail)?,
        ))
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

        Ok(Json(json!({
            "entity": spawned.entity,
            "name": params.name,
            "position": params.position,
        })))
    }

    #[tool(description = "Delete an entity and everything parented to it.")]
    async fn world_despawn_entity(
        &self,
        Parameters(params): Parameters<GetEntityParams>,
    ) -> ToolResult<ResolvedEntity> {
        let identity = params.selector.resolve(&self.brp).await.map_err(fail)?;
        self.brp
            .call_raw("world.despawn_entity", json!({ "entity": identity.entity }))
            .await
            .map_err(fail)?;
        Ok(Json(identity))
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
        Ok(Json(identity))
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
        Ok(Json(identity))
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
        Ok(Json(identity))
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
        Ok(Json(identity))
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
        Ok(Json(PositionResult {
            identity,
            position: result.position,
        }))
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
    use super::reject_position_write;

    const TRANSFORM: &str = "bevy_transform::components::transform::Transform";
    const CELL: &str = "big_space::grid::cell::CellCoord";

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
}
