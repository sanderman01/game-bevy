//! Addressing an entity by name or by id from inside a command.
//!
//! The same rules as the MCP sidecar's selector: exactly one of the two, names match exactly, and
//! an ambiguous name is an error listing the candidates rather than a silent pick. A command that
//! waits on an entity re-resolves it every frame, so this has to live in the game.

use bevy::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::CommandError;

/// Which entity a command acts on. Exactly one of the two fields.
#[derive(Deserialize, Serialize, JsonSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct EntityTarget {
    /// The entity id from an earlier result. Valid only until the game restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<u64>,
    /// The exact value of the entity's `Name`. Must match exactly one entity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl EntityTarget {
    pub fn by_name(name: impl Into<String>) -> Self {
        Self {
            entity: None,
            name: Some(name.into()),
        }
    }

    pub fn by_id(entity: Entity) -> Self {
        Self {
            entity: Some(entity.to_bits()),
            name: None,
        }
    }

    /// Fails with `invalid_params` for zero or two fields, `entity_not_found` for a dead id or an
    /// unused name, and `ambiguous_entity` for a name several entities carry.
    pub fn resolve(&self, world: &mut World) -> Result<Entity, CommandError> {
        match (self.entity, self.name.as_deref()) {
            (Some(_), Some(_)) => Err(CommandError::invalid(
                "invalid_params",
                "give either `entity` or `name`, not both",
            )),
            (None, None) => Err(CommandError::invalid(
                "invalid_params",
                "give either `entity` or `name`",
            )),
            (Some(bits), None) => Entity::try_from_bits(bits)
                .filter(|&entity| world.get_entity(entity).is_ok())
                .ok_or_else(|| {
                    CommandError::failed(
                        "entity_not_found",
                        format!("no entity with id {bits} exists in this run"),
                    )
                    .with_hint(
                        "Ids change every time the game restarts; find the entity again by name.",
                    )
                }),
            (None, Some(name)) => {
                let mut matches: Vec<Entity> = world
                    .query::<(Entity, &Name)>()
                    .iter(world)
                    .filter(|(_, candidate)| candidate.as_str() == name)
                    .map(|(entity, _)| entity)
                    .collect();
                matches.sort();
                match matches[..] {
                    [] => Err(CommandError::failed(
                        "entity_not_found",
                        format!("no entity is named `{name}`"),
                    )),
                    [entity] => Ok(entity),
                    _ => {
                        let ids: Vec<String> =
                            matches.iter().map(|e| e.to_bits().to_string()).collect();
                        Err(CommandError::failed(
                            "ambiguous_entity",
                            format!(
                                "`{name}` is the name of {} entities (ids {})",
                                ids.len(),
                                ids.join(", ")
                            ),
                        )
                        .with_hint("Address one of them by `entity` instead."))
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_a_unique_name_and_a_live_id() {
        let mut world = World::new();
        let crate_ = world.spawn(Name::new("Crate")).id();
        assert_eq!(
            EntityTarget::by_name("Crate").resolve(&mut world),
            Ok(crate_)
        );
        assert_eq!(EntityTarget::by_id(crate_).resolve(&mut world), Ok(crate_));
    }

    #[test]
    fn an_ambiguous_name_lists_the_candidates() {
        let mut world = World::new();
        let a = world.spawn(Name::new("Twin")).id();
        let b = world.spawn(Name::new("Twin")).id();
        let err = EntityTarget::by_name("Twin")
            .resolve(&mut world)
            .unwrap_err();
        assert_eq!(err.kind, "ambiguous_entity");
        assert!(err.message.contains(&a.to_bits().to_string()), "{err}");
        assert!(err.message.contains(&b.to_bits().to_string()), "{err}");
    }

    #[test]
    fn a_stale_id_and_an_unused_name_are_not_found() {
        let mut world = World::new();
        let gone = world.spawn_empty().id();
        world.despawn(gone);
        let err = EntityTarget::by_id(gone).resolve(&mut world).unwrap_err();
        assert_eq!(err.kind, "entity_not_found");
        let err = EntityTarget::by_name("Nobody")
            .resolve(&mut world)
            .unwrap_err();
        assert_eq!(err.kind, "entity_not_found");
    }

    #[test]
    fn zero_or_two_fields_are_invalid() {
        let mut world = World::new();
        let entity = world.spawn(Name::new("A")).id();
        for target in [
            EntityTarget::default(),
            EntityTarget {
                entity: Some(entity.to_bits()),
                name: Some("A".into()),
            },
        ] {
            let err = target.resolve(&mut world).unwrap_err();
            assert_eq!(err.kind, "invalid_params");
        }
    }
}
