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
//! avian also runs. One difference is deliberate: accumulation restarts at every `Grid`, because
//! big_space applies neither a grid's scale nor anything above the root grid to the grid's
//! contents. Where a grid's `Transform` has a scale other than one and avian's propagation also
//! runs, this one's result is the one that stands. A missing `Transform` is the identity.
//!
//! It only does work where something changed. The starting points are colliders and their
//! ancestors ([`ColliderAncestor`], kept by an observer on `ColliderOf`) whose `Transform`,
//! `ChildOf` or `ColliderOf` changed since the last run; from each, it walks down only through
//! marked entities. An idle tick costs one change-tick scan over those entities: with 20,000
//! resting static colliders in a dev build, frame time did not measurably differ from running
//! without this system, where the previous per-collider walk cost about 1.3 ms a frame.
//! A start also walks up to its grid to find the accumulator there, and skips itself when a moved
//! ancestor's walk will cover it.

use avian3d::prelude::{ColliderOf, ColliderTransform, RigidBody, Rotation};
use bevy::prelude::*;
use big_space::prelude::Grid;

/// On every entity between a collider and the grid above it, so the propagation can start at a
/// changed ancestor and walk down to colliders only. Never removed: a stale marker costs a wasted
/// walk when that entity changes, nothing else.
#[derive(Component)]
pub(crate) struct ColliderAncestor;

pub(crate) fn mark_collider_ancestors(
    insert: On<Insert, ColliderOf>,
    mut commands: Commands,
    parents: Query<&ChildOf>,
    grids: Query<(), With<Grid>>,
    marked: Query<(), With<ColliderAncestor>>,
) {
    for ancestor in parents.iter_ancestors(insert.entity) {
        if grids.contains(ancestor) {
            break;
        }
        if !marked.contains(ancestor) {
            commands.entity(ancestor).try_insert(ColliderAncestor);
        }
    }
}

type Node = (
    Option<Ref<'static, Transform>>,
    Option<Ref<'static, ChildOf>>,
    Option<Ref<'static, ColliderOf>>,
    Option<&'static Children>,
    Has<RigidBody>,
    Has<Grid>,
    Has<ColliderAncestor>,
);

type NodeItem<'w, 's> = bevy::ecs::query::QueryItem<'w, 's, Node>;

type Marked = Or<(With<ColliderOf>, With<ColliderAncestor>)>;
type Moved = Or<(Changed<Transform>, Changed<ChildOf>, Changed<ColliderOf>)>;

pub(crate) fn propagate_collider_transforms(
    starts: Query<Entity, (Marked, Moved)>,
    nodes: Query<Node>,
    mut colliders: Query<&mut ColliderTransform, With<ColliderOf>>,
) {
    for start in &starts {
        let Ok(node) = nodes.get(start) else {
            continue;
        };
        let parent = node.1.as_ref().map(|child_of| child_of.parent());
        let (above, above_moved) = parent.map_or((Accumulated::RESET, false), |parent| {
            accumulated_at(parent, &nodes)
        });
        // A moved ancestor is a start of its own, and its walk covers this one.
        if above_moved {
            continue;
        }
        visit(start, above.then(&node), &nodes, &mut colliders);
    }
}

/// avian's propagation accumulator: the transform relative to the nearest rigid body above, valid
/// once `below_body` is set.
#[derive(Clone, Copy)]
struct Accumulated {
    transform: ColliderTransform,
    below_body: bool,
}

impl Accumulated {
    const RESET: Self = Self {
        transform: ColliderTransform {
            translation: Vec3::ZERO,
            rotation: Rotation::IDENTITY,
            scale: Vec3::ONE,
        },
        below_body: false,
    };

    /// The accumulator at a child entity, given this one at its parent.
    fn then(self, (transform, _, _, _, is_body, is_grid, _): &NodeItem<'_, '_>) -> Self {
        if *is_grid {
            return Self::RESET;
        }
        let local = transform.as_deref().copied().unwrap_or_default();
        let scale = self.transform.scale * local.scale;
        if *is_body {
            return Self {
                transform: ColliderTransform { scale, ..default() },
                below_body: true,
            };
        }
        Self {
            transform: ColliderTransform {
                translation: self.transform.transform_point(local.translation),
                rotation: Rotation(self.transform.rotation.0 * local.rotation),
                scale,
            },
            below_body: self.below_body,
        }
    }
}

/// The accumulator at `entity`, built from the nearest grid (or root) down, and whether any marked
/// entity on the way moved since the last run.
fn accumulated_at(entity: Entity, nodes: &Query<Node>) -> (Accumulated, bool) {
    let Ok(node) = nodes.get(entity) else {
        return (Accumulated::RESET, false);
    };
    if node.5 {
        return (Accumulated::RESET, false);
    }
    let parent = node.1.as_ref().map(|child_of| child_of.parent());
    let (above, above_moved) = parent.map_or((Accumulated::RESET, false), |parent| {
        accumulated_at(parent, nodes)
    });
    (above.then(&node), above_moved || moved(&node))
}

fn moved((transform, child_of, collider_of, _, _, _, marked): &NodeItem<'_, '_>) -> bool {
    (*marked || collider_of.is_some())
        && (transform.as_ref().is_some_and(Ref::is_changed)
            || child_of.as_ref().is_some_and(Ref::is_changed)
            || collider_of.as_ref().is_some_and(Ref::is_changed))
}

/// Writes `accumulated` to `entity` if it is a collider on a body, then walks down its marked
/// children.
fn visit(
    entity: Entity,
    accumulated: Accumulated,
    nodes: &Query<Node>,
    colliders: &mut Query<&mut ColliderTransform, With<ColliderOf>>,
) {
    if accumulated.below_body
        && let Ok(mut collider_transform) = colliders.get_mut(entity)
    {
        collider_transform.set_if_neq(accumulated.transform);
    }
    let Ok((.., Some(children), _, _, _)) = nodes.get(entity) else {
        return;
    };
    for child in children {
        let Ok(node) = nodes.get(*child) else {
            continue;
        };
        if node.2.is_none() && !node.6 {
            continue;
        }
        visit(*child, accumulated.then(&node), nodes, colliders);
    }
}
