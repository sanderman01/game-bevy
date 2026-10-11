//! `ename_editor` -- the egui editor: dock layout, selection, viewport, console, gizmo
//! integration.
//!
//! Sits above `ename_engine` and links into a target only when that target asks for it. A
//! shipping build does not contain this crate, and it must never depend on `example_game_lib`. See
//! `docs/design/crate-layout.md`.
//!
//! Based on the example at:
//! <https://github.com/jakobhellermann/bevy-inspector-egui/blob/main/crates/bevy-inspector-egui/examples/integrations/egui_dock.rs>.

mod camera;
mod console;
mod game_view;
mod gizmo;
mod menu;
mod panels;
mod selection;
mod viewport;

use bevy::{app::PluginGroupBuilder, prelude::*};
use bevy_egui::EguiPostUpdateSet;

/// The editor's own ordering. Declared here so the schedule can be read in one place.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum EditorSystems {
    /// Pointer input has updated the selection. Runs after the gizmo, so a press the gizmo is
    /// consuming does not fall through to the mesh behind the handle.
    Select,
    /// Reads the selection produced by [`EditorSystems::Select`]. Runs after the egui pass,
    /// because panel state from this frame is part of the input.
    ApplySelection,
}

pub use camera::EditorCameraSettings;

/// The editor, as a target adds it. Configure with the `with_*` methods;
/// `EditorPlugins::default()` keeps Bevy's camera defaults.
pub struct EditorPlugins {
    camera: EditorCameraSettings,
}

impl Default for EditorPlugins {
    fn default() -> Self {
        Self {
            camera: EditorCameraSettings {
                far: PerspectiveProjection::default().far,
            },
        }
    }
}

impl EditorPlugins {
    /// Sets how far the editor camera sees before frustum culling, in meters. Bevy's 1000 m by
    /// default; a game with large worlds raises it. Changeable later through
    /// [`EditorCameraSettings`].
    pub fn with_camera_far(mut self, far: f32) -> Self {
        self.camera.far = far;
        self
    }
}

impl PluginGroup for EditorPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(EditorSchedulePlugin)
            .add(panels::PanelsPlugin)
            .add(selection::SelectionPlugin)
            .add(viewport::ViewportPlugin)
            .add(camera::EditorCameraPlugin {
                settings: self.camera,
            })
            .add(game_view::GameViewPlugin)
            .add(gizmo::GizmoPlugin)
    }
}

/// Declares [`EditorSystems`] and where each of its sets sits relative to the gizmo and the
/// egui pass. Registers no systems of its own.
pub(crate) struct EditorSchedulePlugin;

impl Plugin for EditorSchedulePlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(
            PostUpdate,
            (
                EditorSystems::Select.after(bevy::prelude::TransformGizmoSystems),
                EditorSystems::ApplySelection.after(EguiPostUpdateSet::EndPass),
            )
                .chain(),
        );
    }
}
