//! `game.entities.list`: the one call behind the agent's main discovery tool.
//!
//! BRP's `world.query` can do the filtering, but only against fully-qualified type paths the
//! caller already knows, and it cannot report a position that survives the floating origin.
//! Both are answered here in a single pass over the world.

use bevy::{
    ecs::hierarchy::ChildOf,
    prelude::*,
    remote::{BrpResult, builtin_methods::parse},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const LIST_METHOD: &str = "game.entities.list";

/// How many entities one call returns before it starts truncating. A loaded glTF scene is
/// thousands of entities and an unbounded list is not readable by anything.
const DEFAULT_LIMIT: usize = 100;

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct ListParams {
    /// Case-insensitive substring of the entity's `Name`. Entities without a `Name` never match.
    name_contains: Option<String>,
    /// Case-insensitive substrings of component type paths. An entity must match all of them.
    with_components: Vec<String>,
    /// Restricts the result to descendants of this entity.
    parent: Option<Entity>,
    limit: Option<usize>,
}

#[derive(Serialize)]
pub(crate) struct ListResponse {
    entities: Vec<EntitySummary>,
    /// Entities that matched but were cut by `limit`. Zero means the list is complete.
    truncated: usize,
}

#[derive(Serialize)]
pub(crate) struct EntitySummary {
    entity: Entity,
    name: Option<String>,
    components: Vec<String>,
    /// Metres, absolute. Absent when the entity is not under a grid, which is most of them.
    position: Option<[f64; 3]>,
}

pub(crate) fn list(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: ListParams = params.map(parse).transpose()?.unwrap_or_default();
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    let name_needle = params.name_contains.map(|n| n.to_lowercase());
    let component_needles: Vec<String> = params
        .with_components
        .iter()
        .map(|c| c.to_lowercase())
        .collect();

    let mut matched = Vec::new();
    let mut truncated = 0usize;

    for entity in world.iter_entities().map(|e| e.id()).collect::<Vec<_>>() {
        let name = world.get::<Name>(entity).map(|n| n.as_str().to_owned());
        if let Some(needle) = &name_needle
            && !name
                .as_ref()
                .is_some_and(|n| n.to_lowercase().contains(needle))
        {
            continue;
        }
        if let Some(parent) = params.parent
            && !is_descendant_of(world, entity, parent)
        {
            continue;
        }

        let components = component_paths(world, entity);
        if !component_needles.iter().all(|needle| {
            components
                .iter()
                .any(|path| path.to_lowercase().contains(needle))
        }) {
            continue;
        }

        if matched.len() == limit {
            truncated += 1;
            continue;
        }
        matched.push(EntitySummary {
            entity,
            name,
            position: crate::position::absolute_position(world, entity)
                .ok()
                .map(|p| p.to_array()),
            components,
        });
    }

    crate::position::to_value(ListResponse {
        entities: matched,
        truncated,
    })
}

/// Type paths of every component on the entity, sorted so two calls agree.
fn component_paths(world: &World, entity: Entity) -> Vec<String> {
    let Ok(entity_ref) = world.get_entity(entity) else {
        return Vec::new();
    };
    let mut paths: Vec<String> = entity_ref
        .archetype()
        .components()
        .iter()
        .filter_map(|&id| world.components().get_info(id))
        .map(|info| info.name().to_string())
        .collect();
    paths.sort_unstable();
    paths
}

fn is_descendant_of(world: &World, entity: Entity, ancestor: Entity) -> bool {
    let mut current = entity;
    while let Some(parent) = world.get::<ChildOf>(current) {
        current = parent.parent();
        if current == ancestor {
            return true;
        }
    }
    false
}
