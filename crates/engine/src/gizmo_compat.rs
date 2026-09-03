//! Transform gizmo compatibility for big_space grids.
//!
//! Bevy's transform gizmo snapshots the dragged entity's local [`Transform`] when the drag
//! starts, then rewrites it every frame as `start_transform.translation + world_delta`.
//! That snapshot is expressed in the grid cell the entity was in at the time. As soon as a
//! drag carries the entity far enough for big_space to recenter it into the next cell, the
//! snapshot is a whole cell stale, so the next frame places the entity a cell's width away
//! -- which recenters it again, and it runs off across the grid a cell per frame.
//!
//! Same class of problem as [`crate::physics_compat`], and the same shape of fix: read the
//! offset big_space applied and put it through to the value that is out of date.
//!
//! # Ordering assumption
//!
//! This corrects the snapshot for a shift that has already happened, so it assumes the
//! gizmo writes `Transform` before `CellCoord::recenter_large_transforms` reads it.
//! Nothing pins that: `transform_gizmo_drag` is only ordered
//! `before(TransformSystems::Propagate)` and the recentering system only ahead of
//! `LocalFloatingOrigin::compute_all`, so the two are unordered and the scheduler picks.
//! The runaway this fixes is the signature of the drag-first order. Neither system is
//! public enough to constrain -- `TransformGizmoSystems` also holds the hover system,
//! which runs *after* propagation, so ordering the whole set ahead of recentering is a
//! cycle. If a future bevy or big_space flips the order, expect a one-cell jump rather
//! than a runaway, and revisit this.

use bevy::prelude::*;
use big_space::prelude::*;

/// Keeps the gizmo's drag snapshot in the cell the dragged entity is currently in.
///
/// `CellCoord::recenter_large_transforms` moves an entity by some `delta` cells and takes
/// `delta * cell_edge_length` off its local translation. The snapshot has to lose the same
/// amount, or the gizmo keeps solving for a position in the cell the entity just left.
///
/// Only the entity's own cell is corrected here. The gizmo's other cached values are
/// world-space and stay valid, since an entity changing cell does not move the floating
/// origin. A drag that outlives a shift of the floating origin itself would still jump,
/// but that needs the camera to fly while a gizmo handle is held.
pub fn reanchor_gizmo_drag_across_cells(
    mut gizmo: ResMut<TransformGizmoState>,
    dragged: Query<(&CellCoord, &ChildOf)>,
    grids: Query<&Grid>,
    mut last_cell: Local<Option<(Entity, CellCoord)>>,
) {
    let Some((entity, cell, grid)) =
        gizmo
            .active
            .then_some(gizmo.entity)
            .flatten()
            .and_then(|entity| {
                let (cell, child_of) = dragged.get(entity).ok()?;
                let grid = grids.get(child_of.parent()).ok()?;
                Some((entity, *cell, grid))
            })
    else {
        *last_cell = None;
        return;
    };

    if let Some((last_entity, last)) = *last_cell
        && last_entity == entity
        && last != cell
    {
        let shift = (cell - last).as_dvec3(grid).as_vec3();
        gizmo.start_transform.translation -= shift;
    }

    *last_cell = Some((entity, cell));
}
