//! Addressing an entity by name or by id.
//!
//! A Bevy `Entity` is a generation-and-index bit pattern that changes every run, so an id is a
//! handle valid only within one run of the game. Names survive restarts and are what a written
//! plan or a message to the user can refer to. Neither is sufficient alone: an entity need not
//! have a `Name`, and names are not unique.

use std::collections::HashMap;

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::brp::BrpClient;

/// The type path of `Name`, the one component an id has to be resolved against.
const NAME: &str = "bevy_ecs::name::Name";

/// Which entity a tool should act on. Exactly one of the two fields.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct EntitySelector {
    /// The entity id from an earlier result. Valid only until the game restarts; an id from a
    /// previous run may silently name a different entity, so compare `pid` before reusing one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<u64>,
    /// The exact value of the entity's `Name` component. Must match exactly one entity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// An entity that has been resolved to one concrete id, carrying its name so results can report
/// both. This is the only way a tool ever names an entity back to the caller.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct ResolvedEntity {
    pub entity: u64,
    pub name: Option<String>,
}

/// One row of `game.entities.list`.
#[derive(Clone, Debug, Deserialize)]
pub struct Summary {
    pub entity: u64,
    pub name: Option<String>,
    #[serde(default)]
    pub position: Option<[f64; 3]>,
}

#[derive(Debug, Deserialize)]
pub struct ListResponse {
    pub entities: Vec<Summary>,
    #[serde(default)]
    pub truncated: usize,
}

impl From<Summary> for ResolvedEntity {
    fn from(summary: Summary) -> Self {
        Self {
            entity: summary.entity,
            name: summary.name,
        }
    }
}

/// What `world.get_components` answers with when `strict` is off. `errors` is not read here: a
/// path that lands there is a component the entity does not have, which for `Name` is an answer
/// and not a failure. Declared locally rather than shared with the tool catalogue, because the
/// two uses want different halves of the response.
#[derive(Default, Deserialize)]
#[serde(default)]
struct ComponentValues {
    components: HashMap<String, Value>,
}

/// Every entity in the world, which is what a name search and `world_grid` walk.
///
/// The limit is high enough not to bind: a name or an id that matched something outside it would
/// read as "no such entity", which is the one answer this must never invent.
async fn all_entities(
    brp: &BrpClient,
    name_contains: Option<&str>,
) -> anyhow::Result<Vec<Summary>> {
    let response: ListResponse = brp
        .call(
            "game.entities.list",
            json!({
                "name_contains": name_contains,
                "include_internal": true,
                "limit": 100_000,
            }),
        )
        .await?;
    Ok(response.entities)
}

impl EntitySelector {
    /// Turns a selector into one entity.
    ///
    /// An ambiguous name is an error listing every candidate rather than a silent pick of the
    /// first match: picking silently would let the agent believe it edited something it did
    /// not, and the scene already contains three entities named `VirtualCamera`.
    ///
    /// An id is already the integer BRP wants, so resolving one is a single read of `Name`
    /// rather than a walk of the world. That read is not redundant: `world.get_components`
    /// looks the entity up before it looks at the component list, so a dead id fails here
    /// instead of reaching the tool that would have edited nothing.
    pub async fn resolve(&self, brp: &BrpClient) -> anyhow::Result<ResolvedEntity> {
        match (self.entity, self.name.as_deref()) {
            (Some(_), Some(_)) => bail!("give either `entity` or `name`, not both"),
            (None, None) => bail!("give either `entity` or `name`"),
            (Some(entity), None) => {
                let values: ComponentValues = brp
                    .call(
                        "world.get_components",
                        json!({ "entity": entity, "components": [NAME], "strict": false }),
                    )
                    .await
                    .with_context(|| format!("no entity with id {entity} exists in this run"))?;

                // An entity without a `Name` answers with the path under `errors`, leaving
                // `components` empty. That is a nameless entity, not a missing one.
                Ok(ResolvedEntity {
                    entity,
                    name: values
                        .components
                        .get(NAME)
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
            }
            (None, Some(name)) => {
                let mut exact: Vec<Summary> = all_entities(brp, Some(name))
                    .await?
                    .into_iter()
                    .filter(|summary| summary.name.as_deref() == Some(name))
                    .collect();

                match exact.len() {
                    0 => bail!(
                        "no entity is named `{name}`. Use world_query with name_contains \
                         to find the right one."
                    ),
                    1 => Ok(exact.remove(0).into()),
                    _ => {
                        let ids: Vec<String> = exact.iter().map(|s| s.entity.to_string()).collect();
                        bail!(
                            "`{name}` is the name of {} entities (ids {}). Address one of them \
                             by `entity` instead.",
                            ids.len(),
                            ids.join(", ")
                        )
                    }
                }
            }
        }
    }
}
