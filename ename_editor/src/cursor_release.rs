//! Shift+Escape frees a cursor the game grabbed, so the editor's panels can be used again.
//!
//! Plain Escape is left to the game, for its own menus.

use bevy::{
    input::InputSystems,
    prelude::*,
    window::{CursorGrabMode, CursorOptions, PrimaryWindow},
};

/// Releases the grabbed cursor on Shift+Escape.
pub(crate) struct CursorReleasePlugin;

impl Plugin for CursorReleasePlugin {
    fn build(&self, app: &mut App) {
        // After this frame's key presses are known.
        app.add_systems(PreUpdate, release_cursor.after(InputSystems));
    }
}

fn release_cursor(
    keys: Res<ButtonInput<KeyCode>>,
    mut cursor: Single<&mut CursorOptions, With<PrimaryWindow>>,
) {
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    if !shift || !keys.just_pressed(KeyCode::Escape) || cursor.grab_mode == CursorGrabMode::None {
        return;
    }
    cursor.grab_mode = CursorGrabMode::None;
    cursor.visible = true;
}
