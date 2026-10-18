//! `ename_editor` -- the egui editor: dock layout, selection, viewport, console, gizmo
//! integration, the `editor.get_view` / `editor.set_view` commands that switch between Scene
//! and Game View, the `camera.*` and `viewpoint.*` commands that pose a camera, the `capture.*`
//! sequences that capture several poses in one job, and the editor camera's memory across
//! restarts.
//!
//! Sits above `ename_engine` and links into a target only when that target asks for it. A
//! shipping build does not contain this crate, and it must never depend on `example_game_lib`. See
//! `docs/design/crate-layout.md`.
//!
//! Based on the example at:
//! <https://github.com/jakobhellermann/bevy-inspector-egui/blob/main/crates/bevy-inspector-egui/examples/integrations/egui_dock.rs>.

mod camera;
mod camera_commands;
mod capture_sequence;
mod console;
mod debug_switches;
mod diagnostics;
mod game_view;
mod gizmo;
mod input_commands;
mod input_inject;
mod menu;
mod panels;
mod selection;
mod view_commands;
mod viewpoints;
mod viewport;

use std::path::PathBuf;

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
#[derive(Default)]
pub struct EditorPlugins {
    camera: EditorCameraSettings,
    viewpoints_file: Option<PathBuf>,
}

impl EditorPlugins {
    /// Sets the project's viewpoints file, the named camera poses `viewpoint.*` read and write.
    /// None by default: the commands then fail `no_viewpoints_file`.
    pub fn with_viewpoints_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.viewpoints_file = Some(path.into());
        self
    }

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
            .add(debug_switches::EditorDebugSwitchesPlugin)
            .add(diagnostics::EditorDiagnosticsPlugin)
            .add(panels::PanelsPlugin)
            .add(selection::SelectionPlugin)
            .add(viewport::ViewportPlugin)
            .add(camera::EditorCameraPlugin {
                settings: self.camera,
            })
            .add(game_view::GameViewPlugin)
            .add(gizmo::GizmoPlugin)
            .add(view_commands::ViewCommandsPlugin)
            .add(camera_commands::CameraCommandsPlugin)
            .add(viewpoints::ViewpointsPlugin {
                file: self.viewpoints_file,
            })
            .add(capture_sequence::CaptureSequencePlugin)
            .add(input_inject::InputInjectPlugin)
            .add(input_commands::InputCommandsPlugin)
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
