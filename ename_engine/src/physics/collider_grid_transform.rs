//! Collider transforms for big_space grids.
//!
//! A collider's [`ColliderTransform`] is its pose relative to its rigid body. avian sets it in two
//! ways, and neither works in a grid:
//! - Every insert of `ColliderOf` (a collider spawned, or a `RigidBody` inserted or replaced
//!   anywhere above it) seeds it from the two entities' `GlobalTransform`s. Those are stale or
//!   cell-less for anything spawned or moved since the last big_space propagation, which puts the
//!   collider cells away from its body, or resets its scale to the body's.
//! - Its propagation walks down from a hierarchy root that has a `Transform`. big_space's root
//!   bundle has none, and neither does a stage's root grid or its load container, so under a stage
//!   that propagation never runs and a child collider's later `Transform` edits never arrive.
//!
//! [`propagate_collider_transforms`] does that propagation itself, in `PhysicsSystems::Prepare`
//! after avian's, so the step and the mass properties see the result.
//!
//! It composes the way avian does, which never involves the cell: the body contributes only its
//! scale accumulated from above, and each entity below it adds translation, rotation and scale
//! separately (componentwise scale, not a decomposed affine, which would shear under non-uniform
//! scale). Scale accumulates top-down, as avian's does, so the two agree bit for bit wherever
//! avian also runs. A missing `Transform` is the identity.
//!
//! Grids follow big_space's rule, the same one `bigspace::camera` places followers with
//! (`bigspace::grid::translation_in_parent`): an entity directly in a grid is
//! placed through its cell, and a grid's scale applies to the grid entity itself but never to its
//! contents. So the owning body is kept across grid boundaries: a vehicle whose body is a grid,
//! with sub-grids and cell-placed holders below it, gets every collider's offset through every
//! cell and grid rotation on the way, and the vehicle's own collider gets its scale.
//!
//! Where avian's propagation also runs and disagrees (under a root grid whose `Transform` has a
//! scale other than one), any `ColliderTransform` avian rewrote is corrected again in the same
//! tick. avian rewrites a moving body's colliders on every tick, so there the correction, and the
//! mass property recompute it triggers, also happen every tick. Stage roots carry no `Transform`,
//! so avian's propagation does not run under them.
//!
//! It only does work where something changed. The starting points are colliders and their
//! ancestors ([`ColliderAncestor`]) whose `Transform`, `ChildOf`, `CellCoord` or `ColliderOf`
//! changed, and colliders whose `ColliderTransform` someone else rewrote, since the last run; from
//! each, it walks down only through marked entities. An idle tick costs one change-tick scan over
//! those entities: with 20,000 resting static colliders in a dev build, frame time did not
//! measurably differ from running without this system, where a per-collider walk cost about
//! 1.3 ms a frame. A start also walks up to the root to find the accumulator there, and skips
//! itself when the walk of a moved ancestor is certain to reach it.

use avian3d::prelude::{ColliderOf, ColliderTransform, RigidBody, Rotation};
use bevy::{ecs::query::QueryData, prelude::*};
use big_space::prelude::{CellCoord, Grid};

use crate::bigspace::grid::translation_in_parent;

/// On every ancestor of a collider, grids included (a body can be a grid, with grids below it),
/// so the propagation can start at a changed ancestor and walk down to colliders only. Kept
/// current when a collider gains its body and when a collider or marked entity is reparented.
/// Never removed: a stale marker costs a wasted walk when that entity changes, nothing else.
///
/// Re-inserted on a collider or marked entity that loses its parent, which makes it a start:
/// nothing else changes on it then, but its subtree's accumulator does.
#[derive(Component)]
pub(crate) struct ColliderAncestor;

pub(crate) fn mark_ancestors_of_new_collider(
    insert: On<Insert, ColliderOf>,
    commands: Commands,
    parents: Query<&ChildOf>,
    marked: Query<(), With<ColliderAncestor>>,
) {
    mark_ancestors(insert.entity, commands, &parents, &marked);
}

pub(crate) fn restart_detached(
    remove: On<Remove, ChildOf>,
    mut commands: Commands,
    walked: Query<(), Marked>,
) {
    if walked.contains(remove.entity) {
        // `try_`: the entity may be despawning.
        commands.entity(remove.entity).try_insert(ColliderAncestor);
    }
}

pub(crate) fn mark_ancestors_of_reparented(
    insert: On<Insert, ChildOf>,
    commands: Commands,
    parents: Query<&ChildOf>,
    marked: Query<(), With<ColliderAncestor>>,
    colliders: Query<(), With<ColliderOf>>,
) {
    let entity = insert.entity;
    if marked.contains(entity) || colliders.contains(entity) {
        mark_ancestors(entity, commands, &parents, &marked);
    }
}

fn mark_ancestors(
    entity: Entity,
    mut commands: Commands,
    parents: &Query<&ChildOf>,
    marked: &Query<(), With<ColliderAncestor>>,
) {
    for ancestor in parents.iter_ancestors(entity) {
        if !marked.contains(ancestor) {
            commands.entity(ancestor).try_insert(ColliderAncestor);
        }
    }
}

#[derive(QueryData)]
pub(crate) struct Node {
    transform: Option<Ref<'static, Transform>>,
    child_of: Option<Ref<'static, ChildOf>>,
    collider_of: Option<Ref<'static, ColliderOf>>,
    children: Option<&'static Children>,
    cell: Option<Ref<'static, CellCoord>>,
    grid: Option<&'static Grid>,
    is_body: Has<RigidBody>,
    marked: Option<Ref<'static, ColliderAncestor>>,
}

impl NodeItem<'_, '_> {
    /// Whether a walk descends into this entity.
    fn on_walk(&self) -> bool {
        self.marked.is_some() || self.collider_of.is_some()
    }

    /// Whether this entity is a start that is not a mere `ColliderTransform` rewrite.
    fn moved(&self) -> bool {
        self.on_walk()
            && (self.transform.as_ref().is_some_and(Ref::is_changed)
                || self.child_of.as_ref().is_some_and(Ref::is_changed)
                || self.cell.as_ref().is_some_and(Ref::is_changed)
                || self.marked.as_ref().is_some_and(Ref::is_changed)
                || self.collider_of.as_ref().is_some_and(Ref::is_changed))
    }

    fn parent(&self) -> Option<Entity> {
        self.child_of.as_ref().map(|child_of| child_of.parent())
    }
}

type Marked = Or<(With<ColliderOf>, With<ColliderAncestor>)>;
type Moved = Or<(
    Changed<Transform>,
    Changed<ChildOf>,
    Changed<CellCoord>,
    Changed<ColliderOf>,
    Changed<ColliderAncestor>,
)>;
/// Colliders whose `ColliderTransform` someone other than this system wrote since its last run.
type Rewritten = (With<ColliderOf>, Changed<ColliderTransform>);
type ColliderTransforms<'w, 's> = Query<'w, 's, &'static mut ColliderTransform, With<ColliderOf>>;

pub(crate) fn propagate_collider_transforms(
    moved: Query<Entity, (Marked, Moved)>,
    nodes: Query<Node>,
    mut colliders: ParamSet<(Query<Entity, Rewritten>, ColliderTransforms)>,
) {
    // Usually empty, so usually no allocation.
    let rewritten: Vec<Entity> = colliders.p0().iter().collect();
    let mut colliders = colliders.p1();
    for start in moved.iter().chain(rewritten) {
        let Ok(node) = nodes.get(start) else {
            continue;
        };
        let (above, reached) = node.parent().map_or((Accumulated::RESET, false), |parent| {
            accumulated_at(parent, &nodes)
        });
        if reached {
            continue;
        }
        let parent_grid = parent_grid(&node, &nodes);
        visit(
            start,
            above.then(parent_grid, &node),
            &nodes,
            &mut colliders,
        );
    }
}

/// avian's propagation accumulator: an entity's pose relative to the nearest rigid body above it
/// (scale relative to the nearest grid above that), valid once `below_body` is set. Each level is
/// placed with big_space's grid rule ([`translation_in_parent`]) and composed componentwise as avian
/// does (`Transform` multiplication), with a grid's own scale left out of its contents' frame.
#[derive(Clone, Copy)]
struct Accumulated {
    pose: Transform,
    below_body: bool,
    is_grid: bool,
}

impl Accumulated {
    const RESET: Self = Self {
        pose: Transform::IDENTITY,
        below_body: false,
        is_grid: false,
    };

    /// The accumulator at `node`, given this one at its parent, which is `parent_grid` if the
    /// parent is a grid.
    fn then(self, parent_grid: Option<&Grid>, node: &NodeItem<'_, '_>) -> Self {
        let local = node.transform.as_deref().copied().unwrap_or_default();
        let cell = node.cell.as_deref().copied().unwrap_or_default();
        let frame = if self.is_grid {
            Transform {
                scale: Vec3::ONE,
                ..self.pose
            }
        } else {
            self.pose
        };
        let translation = translation_in_parent(parent_grid, &cell, &local).as_vec3();
        let placed = frame
            * Transform {
                translation,
                ..local
            };
        let (pose, below_body) = if node.is_body {
            (Transform::from_scale(placed.scale), true)
        } else {
            (placed, self.below_body)
        };
        Self {
            pose,
            below_body,
            is_grid: node.grid.is_some(),
        }
    }

    fn collider_transform(&self) -> ColliderTransform {
        ColliderTransform {
            translation: self.pose.translation,
            rotation: Rotation(self.pose.rotation),
            scale: self.pose.scale,
        }
    }
}

fn parent_grid<'a>(node: &NodeItem<'_, '_>, nodes: &'a Query<Node>) -> Option<&'a Grid> {
    node.parent()
        .and_then(|parent| nodes.get(parent).ok())
        .and_then(|parent| parent.grid)
}

/// The accumulator at `entity`, built from the root down, and whether the walk
/// of a moved start at or above `entity` reaches it: a moved start whose walk passes only through
/// entities it descends into.
fn accumulated_at(entity: Entity, nodes: &Query<Node>) -> (Accumulated, bool) {
    let Ok(node) = nodes.get(entity) else {
        return (Accumulated::RESET, false);
    };
    let (above, above_reached) = node.parent().map_or((Accumulated::RESET, false), |parent| {
        accumulated_at(parent, nodes)
    });
    let reached = node.moved() || (above_reached && node.on_walk());
    (above.then(parent_grid(&node, nodes), &node), reached)
}

/// Writes `accumulated` to `entity` if it is a collider on a body, then walks down the children it
/// descends into.
fn visit(
    entity: Entity,
    accumulated: Accumulated,
    nodes: &Query<Node>,
    colliders: &mut ColliderTransforms,
) {
    if accumulated.below_body
        && let Ok(mut collider_transform) = colliders.get_mut(entity)
    {
        collider_transform.set_if_neq(accumulated.collider_transform());
    }
    let Ok(node) = nodes.get(entity) else {
        return;
    };
    let Some(children) = node.children else {
        return;
    };
    for &child in children {
        let Ok(child_node) = nodes.get(child) else {
            continue;
        };
        if !child_node.on_walk() {
            continue;
        }
        visit(
            child,
            accumulated.then(node.grid, &child_node),
            nodes,
            colliders,
        );
    }
}
