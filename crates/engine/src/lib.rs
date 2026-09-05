//! `engine` -- the runtime layer: camera rig, big_space integration, physics glue, input.
//!
//! Bottom of the layer graph, alongside `modloader`. It must never depend on `editor`,
//! `modloader`, or `game`: if engine code needs something from a higher layer, move the shared
//! type down or invert the call into an event. See `docs/design.md`.

pub mod bigspace;
pub mod camera;
pub mod input;
pub mod physics;

use bevy::{app::PluginGroupBuilder, prelude::*, transform::TransformPlugin};
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
}

impl EnginePlugins {
    /// Sets the primary window. This is the part of `DefaultPlugins` a target legitimately
    /// configures.
    pub fn with_window(mut self, window: Window) -> Self {
        self.window = window;
        self
    }
}

impl PluginGroup for EnginePlugins {
    fn build(self) -> PluginGroupBuilder {
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(self.window),
                ..default()
            })
            .disable::<TransformPlugin>()
            .add_group(BigSpaceDefaultPlugins)
            // No longer bundled with BigSpaceDefaultPlugins as of big_space 0.13.
            .add_group(BigSpaceDebugPlugins::default())
            .add(bigspace::BigSpacePlugin)
            .add(camera::VirtualCameraPlugin)
            .add(input::FlyCameraPlugin)
            .add(physics::PhysicsIntegrationPlugin)
    }
}
