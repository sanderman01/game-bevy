//! What is selected, and how a click changes it.

use bevy::{
    color::palettes::tailwind::*,
    picking::pointer::{PointerAction, PointerInput, PointerInteraction},
    prelude::*,
};

use crate::{
    EditorSystems,
    panels::{ActiveViewport, UiState},
};

/// Turns viewport clicks into a selection.
pub(crate) struct SelectionPlugin;

impl Plugin for SelectionPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MeshPickingPlugin)
            .init_gizmo_group::<EditorGizmos>()
            .add_systems(Update, draw_mesh_intersections)
            .add_systems(PostUpdate, handle_pick_events.in_set(EditorSystems::Select));
    }
}

/// The editor's own gizmos. A group of its own rather than the default one, because the default
/// group is what switches big_space's cell-partition bounds off -- see
/// `ename_engine::debug_overlay`.
#[derive(Default, Reflect, GizmoConfigGroup)]
#[reflect(Default)]
pub(crate) struct EditorGizmos;

fn draw_mesh_intersections(pointers: Query<&PointerInteraction>, mut gizmos: Gizmos<EditorGizmos>) {
    for (point, normal) in pointers
        .iter()
        .filter_map(|interaction| interaction.get_nearest_hit())
        .filter_map(|(_entity, hit)| hit.position.zip(hit.normal))
    {
        gizmos.sphere(point, 0.05, RED_500);
        gizmos.arrow(point, point + normal.normalize() * 0.5, PINK_100);
    }
}

/// Selects the entity under the pointer. The gizmo owns one entity at a time, so a
/// viewport click replaces the selection rather than extending it; the hierarchy panel
/// is where multi-selection still lives.
fn handle_pick_events(
    mut ui_state: ResMut<UiState>,
    mut click_events: MessageReader<PointerInput>,
    pointers: Query<&PointerInteraction>,
    gizmo: Res<TransformGizmoState>,
) {
    if ui_state.active_viewport != ActiveViewport::Scene || !ui_state.pointer_in_viewport {
        return;
    }

    for event in click_events.read() {
        if !matches!(event.action, PointerAction::Press(PointerButton::Primary)) {
            continue;
        }
        // A press the gizmo is consuming must not fall through to the mesh behind the
        // handle. `hovered_axis` is cleared once a drag starts, so both are needed.
        if gizmo.active || gizmo.hovered_axis.is_some() {
            continue;
        }

        for interaction in &pointers {
            if let Some((entity, _)) = interaction.get_nearest_hit() {
                ui_state.selected_entities.select_replace(*entity);
            }
        }
    }
}
