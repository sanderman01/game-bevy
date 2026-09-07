//! `ename_editor` -- the egui editor: dock layout, selection, viewport, gizmo integration.
//!
//! Sits above `ename_engine` and links into a target only when that target asks for it. A
//! shipping build does not contain this crate, and it must never depend on `ename_game`: game
//! specific tooling belongs in `ename_game_editor`. See `docs/design/crate-layout.md`.
//!
//! Based on the example at:
//! <https://github.com/jakobhellermann/bevy-inspector-egui/blob/main/crates/bevy-inspector-egui/examples/integrations/egui_dock.rs>.

mod camera;
mod gizmo;
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

/// The editor, as a target adds it.
pub struct EditorPlugins;

impl PluginGroup for EditorPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(EditorSchedulePlugin)
            .add(panels::PanelsPlugin)
            .add(selection::SelectionPlugin)
            .add(viewport::ViewportPlugin)
            .add(camera::EditorCameraPlugin)
            .add(gizmo::GizmoPlugin)
    }
}

/// Declares [`EditorSystems`] and where each of its sets sits relative to the gizmo and the
/// egui pass. Registers no systems of its own.
struct EditorSchedulePlugin;

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
