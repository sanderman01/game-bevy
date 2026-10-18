//! `game.entities.list`: the one call behind the agent's main discovery tool.
//!
//! BRP's `world.query` can do the filtering, but only against fully-qualified type paths the
//! caller already knows, and it cannot report a position that survives the floating origin.
//! Both are answered here in a single pass over the world.
//!
//! The default also hides the entities Bevy uses to store resources and observers, and sorts
//! named entities first. Neither is cosmetic: without them the first call against this project
//! returns 200 rows of `Messages<WindowMoved>` and not one thing in the scene.

use std::collections::{BTreeMap, HashMap, HashSet};

use bevy::{
    ecs::{component::ComponentId, hierarchy::ChildOf},
    prelude::*,
    remote::{BrpResult, builtin_methods::parse},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const LIST_METHOD: &str = "game.entities.list";

/// How many entities one call returns before it starts truncating. A loaded glTF scene is
/// thousands of entities and an unbounded list is not readable by anything.
const DEFAULT_LIMIT: usize = 50;

/// How many of the most common names a truncated answer counts.
const NAME_COUNTS: usize = 10;

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct ListParams {
    /// Case-insensitive substring of the entity's `Name`. Entities without a `Name` never match.
    name_contains: Option<String>,
    /// Component types. A needle that is some type's short name (any case) matches only types
    /// with that short name; otherwise it is a case-insensitive substring of the type path. An
    /// entity must match all of them.
    with_components: Vec<String>,
    /// Restricts the result to descendants of this entity.
    parent: Option<Entity>,
    /// Include the entities the ECS uses for its own bookkeeping. Off by default: they are
    /// four fifths of the world and none of them are scene content.
    include_internal: bool,
    limit: Option<usize>,
    /// Extra columns: "parent", "children", "components".
    fields: Vec<Field>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Field {
    Position,
    Parent,
    Children,
    Components,
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
    /// Per `with_components` needle, the types it matched. Empty means no type in this world
    /// matches it, not that no entity has it.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    matched_types: BTreeMap<String, Vec<String>>,
    /// When truncated: the most common names among all matches.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    name_counts: Vec<NameCount>,
}

#[derive(Serialize)]
struct NameCount {
    name: String,
    count: usize,
}

#[derive(Serialize)]
pub(crate) struct EntitySummary {
    entity: Entity,
    name: Option<String>,
    /// Metres, absolute, rounded to millimetres. Absent when the entity is not under a grid.
    position: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<Entity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    children: Option<usize>,
    /// Short names.
    #[serde(skip_serializing_if = "Option::is_none")]
    components: Option<Vec<String>>,
}

pub(crate) fn list(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: ListParams = params.map(parse).transpose()?.unwrap_or_default();
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    let name_needle = params.name_contains.map(|n| n.to_lowercase());

    let infos: Vec<(ComponentId, String, String)> = world
        .components()
        .iter_registered()
        .map(|info| {
            let name = info.name();
            (info.id(), name.to_string(), name.shortname().to_string())
        })
        .collect();
    let internal: HashSet<ComponentId> = infos
        .iter()
        .filter(|(_, path, _)| INTERNAL_MARKERS.contains(&path.as_str()))
        .map(|(id, _, _)| *id)
        .collect();
    let mut matched_types = BTreeMap::new();
    let needles: Vec<HashSet<ComponentId>> = params
        .with_components
        .iter()
        .map(|needle| {
            let lower = needle.to_lowercase();
            let exact: Vec<_> = infos
                .iter()
                .filter(|(_, _, short)| short.to_lowercase() == lower)
                .collect();
            let found = if exact.is_empty() {
                infos
                    .iter()
                    .filter(|(_, path, _)| path.to_lowercase().contains(&lower))
                    .collect()
            } else {
                exact
            };
            let mut paths: Vec<String> = found.iter().map(|(_, path, _)| path.clone()).collect();
            paths.sort();
            matched_types.insert(needle.clone(), paths);
            found.iter().map(|(id, _, _)| *id).collect()
        })
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
        let entity_ref = world.entity(entity);
        let components = entity_ref.archetype().components();
        if !params.include_internal && components.iter().any(|id| internal.contains(id)) {
            continue;
        }
        if !needles
            .iter()
            .all(|set| components.iter().any(|id| set.contains(id)))
        {
            continue;
        }
        matched.push((entity, name));
    }

    // Named first, then by id. Archetype iteration order is arbitrary and changes as the world
    // does, so without this `limit` truncates a different, mostly anonymous, set every call.
    matched.sort_by(|a, b| {
        a.1.is_none()
            .cmp(&b.1.is_none())
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.0.cmp(&b.0))
    });

    let truncated = matched.len().saturating_sub(limit);
    let name_counts = if truncated > 0 {
        name_counts(&matched)
    } else {
        Vec::new()
    };
    matched.truncate(limit);
    let shorts: HashMap<ComponentId, &str> = infos
        .iter()
        .map(|(id, _, short)| (*id, short.as_str()))
        .collect();
    let entities = matched
        .into_iter()
        .map(|(entity, name)| summary(world, entity, name, &params.fields, &shorts))
        .collect();
    crate::to_value(ListResponse {
        entities,
        truncated,
        matched_types,
        name_counts,
    })
}

fn summary(
    world: &World,
    entity: Entity,
    name: Option<String>,
    fields: &[Field],
    shorts: &HashMap<ComponentId, &str>,
) -> EntitySummary {
    let wants = |field| fields.contains(&field);
    EntitySummary {
        entity,
        name,
        position: ename_engine::bigspace::grid::absolute_position(world, entity)
            .ok()
            .map(|found| {
                found
                    .position
                    .to_array()
                    .map(|v| (v * 1000.0).round() / 1000.0)
            }),
        parent: wants(Field::Parent)
            .then(|| world.get::<ChildOf>(entity).map(ChildOf::parent))
            .flatten(),
        children: wants(Field::Children)
            .then(|| world.get::<Children>(entity).map_or(0, |c| c.len())),
        components: wants(Field::Components).then(|| {
            let mut names: Vec<String> = world
                .entity(entity)
                .archetype()
                .components()
                .iter()
                .filter_map(|id| shorts.get(id).map(|s| (*s).to_owned()))
                .collect();
            names.sort();
            names
        }),
    }
}

fn name_counts(matched: &[(Entity, Option<String>)]) -> Vec<NameCount> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for (_, name) in matched {
        *counts.entry(name.as_deref().unwrap_or("")).or_default() += 1;
    }
    let mut counts: Vec<NameCount> = counts
        .into_iter()
        .map(|(name, count)| NameCount {
            name: name.to_owned(),
            count,
        })
        .collect();
    counts.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    counts.truncate(NAME_COUNTS);
    counts
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
