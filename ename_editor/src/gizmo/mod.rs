//! Transform gizmo integration.

mod grid_anchor;

use bevy::{gizmos::transform_gizmo::TransformGizmoMeshMarker, picking::Pickable, prelude::*};
use ename_engine::bigspace::GridSystems;

use crate::{
    EditorSystems,
    camera::EditorCamera,
    panels::{ActiveViewport, UiState},
};

/// Everything the transform gizmo needs: which camera it draws through, when it stands down,
/// its keyboard bindings, what it is focused on, and staying anchored across grid cells.
pub(crate) struct GizmoPlugin;

impl Plugin for GizmoPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(TransformGizmoPlugin)
            // The gizmo reads the raw window cursor, so it must stand down while the pointer
            // is over a panel rather than the Game View.
            .configure_sets(PostUpdate, TransformGizmoSystems.run_if(gizmo_should_run))
            .add_systems(
                Update,
                (
                    ignore_gizmo_mesh_picking,
                    tag_editor_camera_for_gizmo,
                    gizmo_keyboard_shortcuts.run_if(not(crate::camera::fly_camera_active)),
                ),
            )
            .add_systems(
                PostUpdate,
                (
                    sync_gizmo_focus.in_set(EditorSystems::ApplySelection),
                    // `GridSystems::Recentered` is the engine's public ordering point:
                    // big_space has moved the entity into its new cell and transforms have
                    // propagated, so the correction lands before the gizmo reads its drag
                    // snapshot again next frame.
                    grid_anchor::reanchor_gizmo_drag_across_cells.in_set(GridSystems::Recentered),
                ),
            );
    }
}

/// The gizmo renders through a camera tagged `TransformGizmoCamera`, and the editor's own scene
/// camera is the one the editor draws into. Tagging it here rather than requiring it on
/// `EditorCamera` keeps the requirement in the crate that has the gizmo: a shipping build has no
/// gizmo at all.
fn tag_editor_camera_for_gizmo(
    cameras: Query<Entity, (With<EditorCamera>, Without<TransformGizmoCamera>)>,
    mut commands: Commands,
) {
    for entity in &cameras {
        commands.entity(entity).insert(TransformGizmoCamera);
    }
}

/// The gizmo takes `window.cursor_position()` directly and knows nothing about the egui panels
/// covering part of the window, so it only runs while the pointer is over Scene View and Scene
/// View is the tab currently showing (not merely over where Scene View's rect was before Game
/// View was selected).
fn gizmo_should_run(ui_state: Res<UiState>, gizmo: Res<TransformGizmoState>) -> bool {
    (ui_state.active_viewport == ActiveViewport::Scene && ui_state.pointer_in_viewport)
        || gizmo.active
}

/// The gizmo renders through an always-on-top overlay camera on its own render layer, and
/// the mesh picking backend builds a ray for that camera too. Without this, clicking a
/// handle selects the handle's mesh entity and paints a debug sphere on it.
fn ignore_gizmo_mesh_picking(
    handles: Query<Entity, Added<TransformGizmoMeshMarker>>,
    mut commands: Commands,
) {
    for entity in &handles {
        commands.entity(entity).insert(Pickable::IGNORE);
    }
}

/// The gizmo reads no keyboard input by design, so the editor picks the bindings: W, E and R
/// select the mode, X toggles between world and local space. The fly camera claims W and E while
/// it is active, so this stands down for it -- see `crate::camera::fly_camera_active`.
fn gizmo_keyboard_shortcuts(
    ui_state: Res<UiState>,
    keys: Res<ButtonInput<KeyCode>>,
    mut settings: ResMut<TransformGizmoSettings>,
) {
    if ui_state.active_viewport != ActiveViewport::Scene || !ui_state.pointer_in_viewport {
        return;
    }

    if keys.just_pressed(KeyCode::KeyW) {
        settings.mode = TransformGizmoMode::Translate;
    }
    if keys.just_pressed(KeyCode::KeyE) {
        settings.mode = TransformGizmoMode::Rotate;
    }
    if keys.just_pressed(KeyCode::KeyR) {
        settings.mode = TransformGizmoMode::Scale;
    }
    if keys.just_pressed(KeyCode::KeyX) {
        settings.space = match settings.space {
            TransformGizmoSpace::World => TransformGizmoSpace::Local,
            TransformGizmoSpace::Local => TransformGizmoSpace::World,
        };
    }
}

/// Points the transform gizmo at the selection.
///
/// The gizmo manipulates exactly one entity, so it is focused only while the selection
/// holds exactly one transformable entity, and cleared otherwise. Deriving focus from the
/// selection every frame, instead of bookkeeping it wherever something selects, is what
/// makes a hierarchy-panel click move the gizmo without a viewport click after it.
fn sync_gizmo_focus(
    ui_state: Res<UiState>,
    transformable: Query<(), With<Transform>>,
    focused: Query<Entity, With<TransformGizmoFocus>>,
    mut commands: Commands,
) {
    let target = match *ui_state.selected_entities.as_slice() {
        [entity] if transformable.contains(entity) => Some(entity),
        _ => None,
    };

    for entity in &focused {
        if Some(entity) != target {
            commands.entity(entity).remove::<TransformGizmoFocus>();
        }
    }

    if let Some(entity) = target
        && !focused.contains(entity)
    {
        commands.entity(entity).insert(TransformGizmoFocus);
    }
}
