//! Addressing an entity by name or by id.
//!
//! A Bevy `Entity` is a generation-and-index bit pattern that changes every run, so an id is a
//! handle valid only within one run of the game. Names survive restarts and are what a written
//! plan or a message to the user can refer to. Neither is sufficient alone: an entity need not
//! have a `Name`, and names are not unique.

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::brp::BrpClient;

/// Which entity a tool should act on. Exactly one of the two fields.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct EntitySelector {
    /// The entity id from an earlier result. Valid only until the game restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<u64>,
    /// The exact value of the entity's `Name` component. Must match exactly one entity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// An entity that has been resolved to a concrete id, carrying its name so results can report
/// both. This is the only way a tool ever names an entity back to the caller.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct ResolvedEntity {
    pub entity: u64,
    pub name: Option<String>,
}

#[derive(Deserialize)]
struct Summary {
    entity: u64,
    name: Option<String>,
}

#[derive(Deserialize)]
struct ListResponse {
    entities: Vec<Summary>,
}

impl EntitySelector {
    /// Turns a selector into one entity id.
    ///
    /// An ambiguous name is an error listing every candidate rather than a silent pick of the
    /// first match: picking silently would let the agent believe it edited something it did
    /// not, and the scene already contains three entities named `VirtualCamera`.
    pub async fn resolve(&self, brp: &BrpClient) -> anyhow::Result<ResolvedEntity> {
        match (self.entity, self.name.as_deref()) {
            (Some(_), Some(_)) => {
                bail!("give either `entity` or `name`, not both")
            }
            (None, None) => bail!("give either `entity` or `name`"),
            (Some(entity), None) => {
                let response: ListResponse = brp
                    .call("game.entities.list", json!({ "limit": 100_000 }))
                    .await?;
                let name = response
                    .entities
                    .into_iter()
                    .find(|summary| summary.entity == entity)
                    .with_context(|| format!("no entity with id {entity} exists in this run"))?
                    .name;
                Ok(ResolvedEntity { entity, name })
            }
            (None, Some(name)) => {
                let response: ListResponse = brp
                    .call(
                        "game.entities.list",
                        json!({ "name_contains": name, "limit": 100_000 }),
                    )
                    .await?;
                let mut exact: Vec<Summary> = response
                    .entities
                    .into_iter()
                    .filter(|summary| summary.name.as_deref() == Some(name))
                    .collect();

                match exact.len() {
                    0 => bail!(
                        "no entity is named `{name}`. Use world_query with name_contains \
                         to find the right one."
                    ),
                    1 => {
                        let found = exact.remove(0);
                        Ok(ResolvedEntity {
                            entity: found.entity,
                            name: found.name,
                        })
                    }
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
