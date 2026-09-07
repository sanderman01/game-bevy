//! Addressing an entity by name or by id.
//!
//! A Bevy `Entity` is an index and a generation, and the pair is reissued every run, so an id is
//! a handle valid only within one run of the game. Names survive restarts and are what a written
//! plan or a message to the user can refer to. Neither is sufficient alone: an entity need not
//! have a `Name`, and names are not unique.
//!
//! Ids cross this boundary as the `{index}v{generation}` string the game prints and the editor's
//! hierarchy panel shows, never as the integer `Entity::to_bits` packs them into. Bevy documents
//! those bits as opaque and they are: the index half is held complemented, so `4294966729` is
//! `566v0`, and no arithmetic outside Bevy should be trusted to know it. The game reports both
//! halves and the packed one stays inside this crate, where BRP's built-in methods need it.

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::brp::BrpClient;

/// An entity id in the form the game prints it: the index, `v`, then the generation.
///
/// Opaque to everything here. It is produced by the game and matched against what the game
/// reports, so this crate never has to know how the two halves are packed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, schemars::JsonSchema)]
pub struct EntityId(String);

impl std::fmt::Display for EntityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl EntityId {
    /// Rejects an id that is not `{index}v{generation}`, before the lookup can report it as a
    /// missing entity instead of a malformed one.
    ///
    /// The packed integer is the mistake worth naming. It is what these tools returned until
    /// recently, it is still what BRP itself speaks, and as a bare number it is a *plausible*
    /// id: `563` would resolve as index 563, generation 0. Failing loudly beats quietly
    /// answering about a different entity.
    fn check_shape(&self) -> anyhow::Result<()> {
        let malformed = || {
            format!(
                "`{self}` is not an entity id. Ids look like `606v0` -- the index, `v`, then the \
                 generation -- as world_query reports them. A bare number is the packed form BRP \
                 uses internally and does not name the same entity."
            )
        };
        let (index, generation) = self.0.split_once('v').with_context(malformed)?;
        let numeric = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        if !numeric(index) || !numeric(generation) {
            bail!(malformed());
        }
        Ok(())
    }
}

/// Which entity a tool should act on. Exactly one of the two fields.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct EntitySelector {
    /// The entity id from an earlier result, e.g. "606v0". Valid only until the game restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<EntityId>,
    /// The exact value of the entity's `Name` component. Must match exactly one entity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// An entity that has been resolved to one concrete id, carrying its name so results can report
/// both. This is the only way a tool ever names an entity back to the caller.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct ResolvedEntity {
    pub entity: EntityId,
    pub name: Option<String>,
    /// The packed form, for calling BRP. Never serialized: it is precisely what `entity` exists
    /// to keep out of the agent's hands.
    #[serde(skip)]
    #[schemars(skip)]
    pub bits: u64,
}

/// How the game reports an entity: the readable id, and the bits its own built-in methods take.
#[derive(Clone, Debug, Deserialize)]
pub struct WireEntity {
    pub id: EntityId,
    pub bits: u64,
}

/// One row of `game.entities.list`.
#[derive(Clone, Debug, Deserialize)]
pub struct Summary {
    pub entity: WireEntity,
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
            entity: summary.entity.id,
            name: summary.name,
            bits: summary.entity.bits,
        }
    }
}

/// Every entity in the world, which is what both halves of `resolve` search.
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
    pub async fn resolve(&self, brp: &BrpClient) -> anyhow::Result<ResolvedEntity> {
        match (self.entity.as_ref(), self.name.as_deref()) {
            (Some(_), Some(_)) => bail!("give either `entity` or `name`, not both"),
            (None, None) => bail!("give either `entity` or `name`"),
            (Some(id), None) => {
                id.check_shape()?;
                all_entities(brp, None)
                    .await?
                    .into_iter()
                    .find(|summary| &summary.entity.id == id)
                    .map(ResolvedEntity::from)
                    .with_context(|| format!("no entity `{id}` exists in this run"))
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
                        let ids: Vec<String> =
                            exact.iter().map(|s| s.entity.id.to_string()).collect();
                        bail!(
                            "`{name}` is the name of {} entities ({}). Address one of them \
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

/// Names an entity the game has just handed back as packed bits, such as a fresh spawn.
pub async fn describe(brp: &BrpClient, bits: u64) -> anyhow::Result<ResolvedEntity> {
    all_entities(brp, None)
        .await?
        .into_iter()
        .find(|summary| summary.entity.bits == bits)
        .map(ResolvedEntity::from)
        .with_context(|| {
            format!(
                "the game reported entity bits {bits}, which it then could not \
                                  find. It may have despawned in the same frame."
            )
        })
}

#[cfg(test)]
mod tests {
    use super::EntityId;

    fn id(s: &str) -> EntityId {
        EntityId(s.to_owned())
    }

    #[test]
    fn accepts_the_form_the_game_prints() {
        for good in ["606v0", "0v0", "4294967294v17"] {
            assert!(id(good).check_shape().is_ok(), "`{good}`");
        }
    }

    /// The packed id is the one that has to fail loudly: as a bare number it would otherwise
    /// look up cleanly as some other entity.
    #[test]
    fn refuses_a_packed_id() {
        for bad in [
            "4294966689",
            "563",
            "",
            "v",
            "606v",
            "v0",
            "606x0",
            "6 0 6v0",
        ] {
            assert!(id(bad).check_shape().is_err(), "`{bad}`");
        }
    }
}
