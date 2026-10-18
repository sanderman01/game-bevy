//! Makes the editor chords of debug switches the live ones, and keeps debug keys from firing
//! while egui has the keyboard.

use bevy::prelude::*;
use bevy_egui::input::egui_wants_any_keyboard_input;
use ename_engine::debug_switch::{DebugSwitchKeys, DebugSwitchTarget};

/// Inserts [`DebugSwitchTarget::Editor`] and gates [`DebugSwitchKeys`] while egui wants keyboard
/// input, so typing F-keys or letters into a text field does not flip a switch.
pub(crate) struct EditorDebugSwitchesPlugin;

impl Plugin for EditorDebugSwitchesPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(DebugSwitchTarget::Editor)
            .configure_sets(
                PreUpdate,
                DebugSwitchKeys.run_if(not(egui_wants_any_keyboard_input)),
            );
    }
}
