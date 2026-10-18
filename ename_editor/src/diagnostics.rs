//! Render diagnostics (GPU timestamp queries every frame) in every editor build, for
//! `diagnostics.log = all` and performance sampling. Release builds do not pay for them.

use bevy::{prelude::*, render::diagnostic::RenderDiagnosticsPlugin};

/// Adds [`RenderDiagnosticsPlugin`] unless the binary added it first.
pub(crate) struct EditorDiagnosticsPlugin;

impl Plugin for EditorDiagnosticsPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<RenderDiagnosticsPlugin>() {
            app.add_plugins(RenderDiagnosticsPlugin);
        }
    }
}
