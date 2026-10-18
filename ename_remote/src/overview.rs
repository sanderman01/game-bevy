//! `game.entities.overview`: the named entities as a tree, a few tags each.
//!
//! One call that orients an agent in a scene, in place of a query plus a component listing per
//! entity. Unnamed entities are not listed: their named descendants move up to their place, and
//! the named parent counts them.

use bevy::{
    ecs::{component::ComponentId, hierarchy::ChildOf},
    prelude::*,
    remote::{BrpResult, builtin_methods::parse},
};
use ename_engine::command::EntityTarget;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::command::brp_error;

pub const METHOD: &str = "game.entities.overview";

const DEFAULT_DEPTH: u32 = 3;
const DEFAULT_LIMIT: usize = 80;
const MAX_TAGS: usize = 8;

/// Components every scene entity has or that say nothing about what it is.
const NOISE: [&str; 22] = [
    "Name",
    "Transform",
    "GlobalTransform",
    "TransformTreeChanged",
    "ChildOf",
    "Children",
    "CellCoord",
    "Visibility",
    "InheritedVisibility",
    "ViewVisibility",
    "VisibilityClass",
    "VisibleEntities",
    "SyncToRenderWorld",
    "RenderEntity",
    "MainEntity",
    "Aabb",
    "Frustum",
    "CameraMainTextureUsages",
    "Msaa",
    "SceneInstance",
    "GltfExtras",
    "GltfMaterialName",
];

/// Components that say what an entity is, listed before any other tag.
const KINDS: [&str; 16] = [
    "Camera3d",
    "Camera2d",
    "Camera",
    "DirectionalLight",
    "PointLight",
    "SpotLight",
    "Mesh3d",
    "SceneRoot",
    "RigidBody",
    "Collider",
    "Grid",
    "Window",
    "AudioPlayer",
    "Node",
    "Text",
    "Sprite",
];

const INTERNAL_MARKERS: [&str; 3] = [
    "bevy_ecs::resource::IsResource",
    "bevy_ecs::observer::distributed_storage::Observer",
    "bevy_ecs::system::system_registry::SystemIdMarker",
];

#[derive(Default, Deserialize)]
#[serde(default)]
struct OverviewParams {
    root: Option<EntityTarget>,
    depth: Option<u32>,
    limit: Option<usize>,
}

#[derive(Serialize)]
struct Row {
    entity: u64,
    name: String,
    depth: u32,
    /// Absolute metres, rounded to millimetres.
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<[f64; 3]>,
    tags: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    unnamed_children: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Serialize)]
struct Overview {
    entities: Vec<Row>,
    /// Named entities left out by `limit` or `depth`.
    truncated: usize,
    /// ECS bookkeeping entities (resources, observers, systems) at the top level, never listed.
    hidden_internal: usize,
}

pub(crate) fn overview(In(raw): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: OverviewParams = raw.map(parse).transpose()?.unwrap_or_default();
    let depth = params.depth.unwrap_or(DEFAULT_DEPTH);
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    let internal: Vec<ComponentId> = world
        .components()
        .iter_registered()
        .filter(|info| INTERNAL_MARKERS.contains(&info.name().to_string().as_str()))
        .map(|info| info.id())
        .collect();
    let is_internal = |world: &World, entity: Entity| {
        world
            .entity(entity)
            .archetype()
            .components()
            .iter()
            .any(|id| internal.contains(id))
    };

    let (roots, hidden_internal) = match &params.root {
        Some(target) => (
            vec![target.resolve(world).map_err(|err| brp_error(err, world))?],
            0,
        ),
        None => {
            let mut roots = Vec::new();
            let mut hidden = 0;
            let top: Vec<Entity> = world
                .query_filtered::<Entity, Without<ChildOf>>()
                .iter(world)
                .collect();
            for entity in top {
                if is_internal(world, entity) {
                    hidden += 1;
                } else {
                    roots.push(entity);
                }
            }
            (roots, hidden)
        }
    };
    let mut walk = Walk {
        rows: Vec::new(),
        truncated: 0,
        limit,
        max_depth: depth,
    };
    for root in sorted(world, roots) {
        walk.visit(world, root, 0);
    }
    crate::to_value(Overview {
        entities: walk.rows,
        truncated: walk.truncated,
        hidden_internal,
    })
}

struct Walk {
    rows: Vec<Row>,
    truncated: usize,
    limit: usize,
    max_depth: u32,
}

impl Walk {
    /// Lists `entity` at `depth` if named, else passes its children up to `depth`.
    fn visit(&mut self, world: &World, entity: Entity, depth: u32) {
        let Some(name) = world.get::<Name>(entity) else {
            for child in sorted(world, children(world, entity)) {
                self.visit(world, child, depth);
            }
            return;
        };
        if depth > self.max_depth || self.rows.len() >= self.limit {
            self.truncated += 1 + named_below(world, entity);
            return;
        }
        let kids = children(world, entity);
        let unnamed = kids
            .iter()
            .filter(|&&child| world.get::<Name>(child).is_none())
            .count();
        self.rows.push(Row {
            entity: entity.to_bits(),
            name: name.as_str().to_owned(),
            depth,
            position: ename_engine::bigspace::grid::absolute_position(world, entity)
                .ok()
                .map(|found| {
                    found
                        .position
                        .to_array()
                        .map(|v| (v * 1000.0).round() / 1000.0)
                }),
            tags: tags(world, entity),
            unnamed_children: unnamed,
        });
        for child in sorted(world, kids) {
            self.visit(world, child, depth + 1);
        }
    }
}

fn children(world: &World, entity: Entity) -> Vec<Entity> {
    world
        .get::<Children>(entity)
        .map(|children| children.iter().collect())
        .unwrap_or_default()
}

fn named_below(world: &World, entity: Entity) -> usize {
    children(world, entity)
        .into_iter()
        .map(|child| usize::from(world.get::<Name>(child).is_some()) + named_below(world, child))
        .sum()
}

/// By name, unnamed last, then by id, so the answer is the same every call.
fn sorted(world: &World, mut entities: Vec<Entity>) -> Vec<Entity> {
    entities.sort_by_cached_key(|&entity| {
        let name = world.get::<Name>(entity).map(|n| n.as_str().to_owned());
        (name.is_none(), name, entity)
    });
    entities
}

fn tags(world: &World, entity: Entity) -> Vec<String> {
    let mut tags: Vec<String> = world
        .entity(entity)
        .archetype()
        .components()
        .iter()
        .filter_map(|&id| world.components().get_info(id))
        .map(|info| info.name().shortname().to_string())
        // Generic markers such as `AncestorMarker<ColliderMarker>` are bookkeeping.
        .filter(|short| !NOISE.contains(&short.as_str()) && !short.contains('<'))
        .collect();
    // What the entity is first, then the rest by name.
    tags.sort_by_key(|tag| {
        let rank = KINDS.iter().position(|kind| kind == tag);
        (rank.is_none(), rank, tag.clone())
    });
    tags.dedup();
    tags.truncate(MAX_TAGS);
    if world.get::<Visibility>(entity) == Some(&Visibility::Hidden) {
        tags.push("Hidden".to_owned());
    }
    tags
}
