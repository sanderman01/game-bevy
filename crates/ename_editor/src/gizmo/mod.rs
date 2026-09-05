//! Transform gizmo integration.

mod grid_anchor;

use bevy::prelude::*;
use ename_engine::bigspace::GridSystems;

/// Keeps the transform gizmo working inside big_space grids.
pub struct GizmoPlugin;

impl Plugin for GizmoPlugin {
    fn build(&self, app: &mut App) {
        // `GridSystems::Recentered` is the engine's public ordering point: big_space has moved
        // the entity into its new cell and transforms have propagated, so the correction lands
        // before the gizmo reads its drag snapshot again next frame.
        app.add_systems(
            PostUpdate,
            grid_anchor::reanchor_gizmo_drag_across_cells.in_set(GridSystems::Recentered),
        );
    }
}
