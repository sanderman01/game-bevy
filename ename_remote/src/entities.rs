//! `game.entities.list`: the one call behind the agent's main discovery tool.
//!
//! BRP's `world.query` can do the filtering, but only against fully-qualified type paths the
//! caller already knows, and it cannot report a position that survives the floating origin.
//! Both are answered here in a single pass over the world.
//!
//! The default also hides the entities Bevy uses to store resources and observers, and sorts
//! named entities first. Neither is cosmetic: without them the first call against this project
//! returns 200 rows of `Messages<WindowMoved>` and not one thing in the scene.

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
    /// Include the entities the ECS uses for its own bookkeeping. Off by default: they are
    /// four fifths of the world and none of them are scene content.
    include_internal: bool,
    limit: Option<usize>,
}

/// Components that mark an entity as ECS bookkeeping rather than scene content.
///
/// Bevy stores resources, observers and registered systems as entities. In this project that is
/// over 500 of them against about a dozen the agent means by "an entity", so listing them by
/// default buries the answer and spends the whole `limit` before reaching anything named.
const INTERNAL_MARKERS: [&str; 3] = [
    "bevy_ecs::resource::IsResource",
    "bevy_ecs::observer::distributed_storage::Observer",
    // One per BRP method, so this crate is otherwise the largest single contributor.
    "bevy_ecs::system::system_registry::SystemIdMarker",
];

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
        if !params.include_internal
            && components
                .iter()
                .any(|path| INTERNAL_MARKERS.contains(&path.as_str()))
        {
            continue;
        }
        if !component_needles.iter().all(|needle| {
            components
                .iter()
                .any(|path| path.to_lowercase().contains(needle))
        }) {
            continue;
        }

        matched.push(EntitySummary {
            entity,
            name,
            position: crate::position::absolute_position(world, entity)
                .ok()
                .map(|p| p.to_array()),
        });
    }

    // Named first, then by id. Archetype iteration order is arbitrary and changes as the world
    // does, so without this `limit` truncates a different, mostly anonymous, set every call.
    matched.sort_by(|a, b| {
        a.name
            .is_none()
            .cmp(&b.name.is_none())
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.entity.cmp(&b.entity))
    });

    let truncated = matched.len().saturating_sub(limit);
    matched.truncate(limit);
    crate::to_value(ListResponse {
        entities: matched,
        truncated,
    })
}

/// Type paths of every component on the entity. Not part of any response: these exist only to
/// answer the `include_internal` and `with_components` membership tests above, so their order
/// is never observed.
fn component_paths(world: &World, entity: Entity) -> Vec<String> {
    let Ok(entity_ref) = world.get_entity(entity) else {
        return Vec::new();
    };
    entity_ref
        .archetype()
        .components()
        .iter()
        .filter_map(|&id| world.components().get_info(id))
        .map(|info| info.name().to_string())
        .collect()
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
