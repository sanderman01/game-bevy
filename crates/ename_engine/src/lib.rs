//! `ename_engine` -- the runtime layer: camera rig, big_space integration, physics glue, input,
//! time control.
//!
//! Bottom of the layer graph, alongside `ename_content`. It must never depend on `ename_editor`,
//! `ename_content`, or `ename_game`: if engine code needs something from a higher layer, move the
//! shared type down or invert the call into an event. See `docs/design/crate-layout.md`.

pub mod bigspace;
pub mod camera;
pub mod input;
pub mod physics;
pub mod time;

use bevy::{
    app::PluginGroupBuilder,
    log::{BoxedLayer, LogPlugin},
    prelude::*,
    transform::TransformPlugin,
};
use big_space::plugin::{BigSpaceDebugPlugins, BigSpaceDefaultPlugins};

/// Everything a target needs to run on this engine, `DefaultPlugins` included.
///
/// The group owns `DefaultPlugins` deliberately: big_space supplies transform propagation, so
/// Bevy's `TransformPlugin` has to be disabled, and a `PluginGroup` cannot disable a plugin
/// belonging to a different group. If a binary assembled `DefaultPlugins` itself, every target
/// would have to remember that call, and forgetting it gives double propagation with no
/// compile error.
#[derive(Default)]
pub struct EnginePlugins {
    window: Window,
    log_layer: Option<fn(&mut App) -> Option<BoxedLayer>>,
}

impl EnginePlugins {
    /// Sets the primary window. This is the part of `DefaultPlugins` a target legitimately
    /// configures.
    pub fn with_window(mut self, window: Window) -> Self {
        self.window = window;
        self
    }

    /// Adds a `tracing` layer alongside the default formatter.
    ///
    /// `LogPlugin` installs the global subscriber during plugin build and a subscriber cannot
    /// gain layers afterwards, so anything that wants to see log events has to be handed in
    /// here rather than adding itself later.
    pub fn with_log_layer(mut self, layer: fn(&mut App) -> Option<BoxedLayer>) -> Self {
        self.log_layer = Some(layer);
        self
    }
}

impl PluginGroup for EnginePlugins {
    fn build(self) -> PluginGroupBuilder {
        let mut plugins = DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(self.window),
                ..default()
            })
            .disable::<TransformPlugin>();

        if let Some(custom_layer) = self.log_layer {
            plugins = plugins.set(LogPlugin {
                custom_layer,
                ..default()
            });
        }

        plugins
            .add_group(BigSpaceDefaultPlugins)
            // No longer bundled with BigSpaceDefaultPlugins as of big_space 0.13.
            .add_group(BigSpaceDebugPlugins::default())
            .add(bigspace::BigSpacePlugin)
            .add(camera::VirtualCameraPlugin)
            .add(input::FlyCameraPlugin)
            .add(physics::PhysicsIntegrationPlugin)
            .add(time::TimeControlPlugin)
    }
}
