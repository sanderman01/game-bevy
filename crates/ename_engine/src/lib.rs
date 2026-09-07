//! `ename_engine` -- the runtime layer: camera rig, big_space integration, physics glue, input,
//! time control, log capture.
//!
//! Bottom of the layer graph, alongside `ename_content`. It must never depend on `ename_editor`,
//! `ename_content`, or `ename_game`: if engine code needs something from a higher layer, move the
//! shared type down or invert the call into an event. See `docs/design/crate-layout.md`.

pub mod bigspace;
pub mod camera;
pub mod input;
pub mod log;
pub mod physics;
pub mod time;

use bevy::{app::PluginGroupBuilder, log::LogPlugin, prelude::*, transform::TransformPlugin};
use big_space::plugin::{BigSpaceDebugPlugins, BigSpaceDefaultPlugins};

/// Everything a target needs to run on this engine, `DefaultPlugins` included.
///
/// The group owns `DefaultPlugins` deliberately: big_space supplies transform propagation, so
/// Bevy's `TransformPlugin` has to be disabled, and a `PluginGroup` cannot disable a plugin
/// belonging to a different group. If a binary assembled `DefaultPlugins` itself, every target
/// would have to remember that call, and forgetting it gives double propagation with no
/// compile error.
pub struct EnginePlugins {
    window: Window,
    log_capacity: usize,
}

impl Default for EnginePlugins {
    fn default() -> Self {
        Self {
            window: Window::default(),
            log_capacity: log::DEFAULT_CAPACITY,
        }
    }
}

impl EnginePlugins {
    /// Sets the primary window. This is the part of `DefaultPlugins` a target legitimately
    /// configures.
    pub fn with_window(mut self, window: Window) -> Self {
        self.window = window;
        self
    }

    /// Sets how many events [`log::LogBuffer`] keeps before it drops the oldest.
    pub fn with_log_capacity(mut self, capacity: usize) -> Self {
        self.log_capacity = capacity;
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
            .set(LogPlugin {
                // The subscriber-wide filter gates every layer, so it is opened all the way and
                // each consumer carries its own: `terminal_layer` for stderr, `log::CaptureLevel`
                // for the buffer.
                level: log::MAX_LEVEL,
                custom_layer: log::capture_layer,
                fmt_layer: log::terminal_layer,
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
            .add(time::TimeControlPlugin)
            // After `DefaultPlugins`, because it resizes the buffer `LogPlugin` just built.
            .add(log::LogBufferPlugin {
                capacity: self.log_capacity,
            })
    }
}
