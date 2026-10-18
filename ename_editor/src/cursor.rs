//! Keeps egui's cursor icon under feathers.
//!
//! feathers' `CursorIconPlugin` (in `EnginePlugins`) writes `DefaultCursor` into the window's
//! `CursorIcon` every `PreUpdate` while no feathers widget is hovered, and bevy_egui writes
//! `CursorIcon` only when egui's icon changes. Without this, egui's resize and text cursors last
//! one frame.

use bevy::{
    feathers::cursor::{DefaultCursor, EntityCursor},
    prelude::*,
    window::SystemCursorIcon,
};
use bevy_egui::{EguiOutput, EguiPostUpdateSet, PrimaryEguiContext, helpers};

/// Copies the primary egui context's cursor icon into feathers' `DefaultCursor`.
pub(crate) struct EguiCursorPlugin;

impl Plugin for EguiCursorPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PostUpdate,
            sync_default_cursor
                .after(EguiPostUpdateSet::ProcessOutput)
                .run_if(resource_exists::<DefaultCursor>),
        );
    }
}

fn sync_default_cursor(
    output: Single<&EguiOutput, With<PrimaryEguiContext>>,
    mut default_cursor: ResMut<DefaultCursor>,
) {
    let icon = helpers::egui_to_winit_cursor_icon(output.platform_output.cursor_icon)
        .unwrap_or(SystemCursorIcon::Default);
    let cursor = EntityCursor::System(icon);
    if default_cursor.0 != cursor {
        default_cursor.0 = cursor;
    }
}
