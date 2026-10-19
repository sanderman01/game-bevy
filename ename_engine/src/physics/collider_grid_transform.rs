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
//! after avian's, so the step and the mass properties see the result. It composes the way avian
//! does, which never involves the cell: the body contributes only its scale accumulated from the
//! root, and each entity below it adds translation, rotation and scale separately (componentwise
//! scale, not a decomposed affine, which would shear under non-uniform scale). A missing
//! `Transform` counts as the identity. A collider is recomputed when its `ColliderOf` was
//! (re)inserted, or when the `Transform` or `ChildOf` of any entity from it up to the root changed.

use avian3d::prelude::{ColliderOf, ColliderTransform, Rotation};
use bevy::prelude::*;

type Link = (
    Option<Ref<'static, Transform>>,
    Option<Ref<'static, ChildOf>>,
);

pub(crate) fn propagate_collider_transforms(
    mut colliders: Query<(Entity, &mut ColliderTransform, Ref<ColliderOf>)>,
    hierarchy: Query<Link>,
) {
    for (collider, mut collider_transform, collider_of) in &mut colliders {
        let Some((computed, changed)) = body_relative(collider, collider_of.body, &hierarchy)
        else {
            continue;
        };
        if changed || collider_of.is_changed() {
            collider_transform.set_if_neq(computed);
        }
    }
}

/// avian's propagation from `body` down to `collider`, and whether anything on the way up to the
/// root changed since the last run. `None` if `collider` is not below `body`.
fn body_relative(
    collider: Entity,
    body: Entity,
    hierarchy: &Query<Link>,
) -> Option<(ColliderTransform, bool)> {
    let mut changed = false;
    let mut link = |entity: Entity| {
        let (transform, child_of) = hierarchy.get(entity).ok()?;
        changed |= transform.as_ref().is_some_and(Ref::is_changed)
            || child_of.as_ref().is_some_and(Ref::is_changed);
        Some((
            transform.as_deref().copied().unwrap_or_default(),
            child_of.map(|child_of| child_of.parent()),
        ))
    };

    let mut below_body = Vec::new();
    let mut current = collider;
    while current != body {
        let (transform, parent) = link(current)?;
        below_body.push(transform);
        current = parent?;
    }

    let mut scale = Vec3::ONE;
    let mut ancestor = Some(body);
    while let Some(entity) = ancestor {
        // A parent despawned this frame ends the walk, as the root does.
        let Some((transform, parent)) = link(entity) else {
            break;
        };
        scale = transform.scale * scale;
        ancestor = parent;
    }

    let mut accumulated = ColliderTransform { scale, ..default() };
    for transform in below_body.into_iter().rev() {
        let child = ColliderTransform::from(transform);
        accumulated = ColliderTransform {
            translation: accumulated.transform_point(child.translation),
            rotation: Rotation(accumulated.rotation.0 * child.rotation.0),
            scale: accumulated.scale * child.scale,
        };
    }
    Some((accumulated, changed))
}
