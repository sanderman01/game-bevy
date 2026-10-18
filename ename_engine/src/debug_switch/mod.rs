//! Debug switches: named toggles and modes for debugging, performance analysis and modding, read
//! and set by name at runtime in every build.
//!
//! A crate registers a switch in its plugin's `build` with [`DebugSwitchAppExt`]: a category, a
//! name, a description, a reflected resource (or one field of it) or a pair of callbacks, and an
//! optional key chord per build target. [`DebugSwitchRegistry`] owns every debug key: one system
//! toggles a bool or cycles an enum when its chord is pressed, so no crate writes its own key
//! system. The `debug.*` commands list, read, set and reset switches; [`DebugSwitchWorldExt`]
//! does the same from Rust.
//!
//! Gameplay controls and player bindings are not debug switches.

mod command;
mod keys;
mod registry;

use bevy::{input::InputSystems, prelude::*};

pub use registry::{
    DebugSwitchAppExt, DebugSwitchError, DebugSwitchRegistry, DebugSwitchSpec, DebugSwitchTarget,
    DebugSwitchValue, DebugSwitchWorldExt, KeyChord,
};

/// Where debug keys are applied, in `PreUpdate` after `InputSystems`, so every `Update` reader
/// sees a change in the frame its key was pressed. The editor gates it while egui has keyboard
/// focus.
#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
pub struct DebugSwitchKeys;

/// Owns [`DebugSwitchRegistry`], [`DebugSwitchTarget`], the key system and the `debug.*`
/// commands. Part of `EnginePlugins`.
pub struct DebugSwitchPlugin;

impl Plugin for DebugSwitchPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DebugSwitchTarget>()
            .configure_sets(PreUpdate, DebugSwitchKeys.after(InputSystems))
            .add_systems(
                PreUpdate,
                keys::apply_debug_keys
                    .in_set(DebugSwitchKeys)
                    .run_if(resource_exists::<ButtonInput<KeyCode>>),
            )
            .add_systems(
                Last,
                registry::record_launch_values.run_if(registry::launch_values_missing),
            );
    }

    /// `finish`, so every switch registered in any plugin's `build` is known, whatever the
    /// plugin order.
    fn finish(&self, app: &mut App) {
        let pending = app
            .world_mut()
            .remove_resource::<registry::PendingDebugSwitches>()
            .unwrap_or_default();
        let registry = DebugSwitchRegistry::build(pending);
        app.insert_resource(registry);
        command::register(app);
    }
}

#[cfg(test)]
mod tests;
