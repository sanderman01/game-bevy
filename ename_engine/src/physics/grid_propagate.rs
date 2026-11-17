//! Grid-aware stand-in for avian's `propagate_before_physics`.
//!
//! The engine runs avian with `transform_to_position`, which copies `GlobalTransform` into
//! `Position` and `Rotation` in `PhysicsSystems::Prepare` unless they changed after the last
//! step. avian's own propagation before that copy is off, because Bevy's propagation does not
//! know big_space cells; big_space propagates only in `PostUpdate`. Without a replacement, the
//! second and later fixed ticks of a frame read last frame's `GlobalTransform`, so every moving
//! body is put back to last frame's pose before it steps, and a `Transform` edit reaches
//! `Position` a frame late.
//!
//! [`propagate_grid_bodies`] recomputes `GlobalTransform` from `Grid::global_transform` for
//! bodies and colliders directly under a `Grid`, just before that copy. The floating origin only
//! moves in `PostUpdate`, so the result is in the same frame as the last propagation, and the
//! first tick after an origin shift moves every `Position` into the new frame exactly.
//!
//! It recomputes `GlobalTransform` rather than writing `Position`, so avian's own rules for the
//! copy still hold: a `Position` or `Rotation` set directly since the last step wins, and
//! differences under avian's tolerance are ignored.
//!
//! Which entities it touches:
//! - every non-static body, every tick;
//! - a static body or bodiless collider only when its `Transform`, `CellCoord` or parent changed;
//! - any entity whose `Position` was just added or that just gained a `RigidBody`. avian seeds
//!   `GlobalTransform` for those from the `Transform` chain without the cell, cells away, and
//!   would copy that into `Position`; a joint to it would then pull across that distance.
//!
//! A `Position` given at spawn loses to `Transform`, as it does in avian whenever
//! `position_to_transform` is off: place grid entities by `CellCoord` and `Transform`.
//!
//! An entity nested under a non-grid parent gets the same cell-less seed when it gains a collider
//! or a body. big_space recomputes such an entity only when its `Transform` changes, so on a
//! settled entity, such as a glTF mesh that gets its collider once its scene is ready, the seed
//! would stay and the mesh would render cells away. [`reseed_nested_global_transform`] puts it
//! back under its parent.
//!
//! Not covered: a nested entity that moves within a frame keeps last frame's pose until
//! `PostUpdate`, and a nested grid that moves within a frame gives its bodies last frame's grid
//! pose, because its floating-origin transform is also only updated in `PostUpdate`.

use avian3d::prelude::{Collider, Position, RigidBody};
use bevy::prelude::*;
use big_space::prelude::{CellCoord, Grid};

type GridEntity = (
    Option<Ref<'static, RigidBody>>,
    Ref<'static, Position>,
    Ref<'static, ChildOf>,
    Ref<'static, CellCoord>,
    Ref<'static, Transform>,
    &'static mut GlobalTransform,
);

/// Bodies, and colliders without one.
type BodyOrCollider = Or<(With<RigidBody>, With<Collider>)>;

pub(crate) fn propagate_grid_bodies(
    grids: Query<&Grid>,
    mut entities: Query<GridEntity, BodyOrCollider>,
) {
    for (body, position, child_of, cell, transform, mut global_transform) in &mut entities {
        let seeded = position.is_added() || body.as_ref().is_some_and(Ref::is_added);
        let moving = body.as_ref().is_some_and(|body| !body.is_static());
        let moved = transform.is_changed() || cell.is_changed() || child_of.is_changed();
        if !(seeded || moving || moved) {
            continue;
        }
        let Ok(grid) = grids.get(child_of.parent()) else {
            continue;
        };
        global_transform.set_if_neq(grid.global_transform(&cell, &transform));
    }
}

/// Undoes avian's cell-less `GlobalTransform` seed on an entity nested under a non-grid parent,
/// from its parent's `GlobalTransform`. Runs after avian's insert hook for `C`.
pub(crate) fn reseed_nested_global_transform<C: Component>(
    add: On<Add<C>>,
    grids: Query<(), With<Grid>>,
    mut entities: Query<(Option<&ChildOf>, Option<&Transform>, &mut GlobalTransform)>,
) {
    let Ok((Some(child_of), Some(&transform), _)) = entities.get(add.entity) else {
        return;
    };
    let parent = child_of.parent();
    // Directly in a grid: placed through its cell by `propagate_grid_bodies`.
    if grids.contains(parent) {
        return;
    }
    let Ok((_, _, &parent_global)) = entities.get(parent) else {
        return;
    };
    if let Ok((_, _, mut global_transform)) = entities.get_mut(add.entity) {
        global_transform.set_if_neq(parent_global.mul_transform(transform));
    }
}
