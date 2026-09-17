//! `ename_engine` -- the runtime layer: camera rig, big_space integration, physics glue, input,
//! time control, log capture.
//!
//! Sits directly above the asset crates. It must never depend on `ename_editor` or `example_game_lib`:
//! if engine code needs something from a higher layer, move the shared type down or invert the
//! call into an event. It does depend on `ename_asset_content`, because the `alias://` source has
//! to be registered before `AssetPlugin` builds and this group owns `AssetPlugin`.
//! See `docs/design/crate-layout.md`.

pub mod bigspace;
pub mod camera;
pub mod debug_overlay;
pub mod log;
pub mod physics;
pub mod stage;
pub mod time;

use bevy::{
    app::PluginGroupBuilder, asset::AssetPlugin, log::LogPlugin, prelude::*,
    transform::TransformPlugin,
};
use big_space::plugin::{BigSpaceDebugPlugins, BigSpaceDefaultPlugins};
use ename_asset_content::AssetContentPlugin;
use std::path::PathBuf;

pub use ename_asset_content::DEFAULT_ASSET_ROOT;
/// Re-exported so a target can name the user's load order file without depending on the asset
/// crates itself. Where that file lives is the binary's decision; what it is called is not.
pub use ename_asset_content::LOAD_ORDER_FILE;

/// Everything a target needs to run on this engine, `DefaultPlugins` included.
///
/// The group owns `DefaultPlugins` deliberately: big_space supplies transform propagation, so
/// Bevy's `TransformPlugin` has to be disabled, and a `PluginGroup` cannot disable a plugin
/// belonging to a different group. If a binary assembled `DefaultPlugins` itself, every target
/// would have to remember that call, and forgetting it gives double propagation with no
/// compile error.
///
/// It also owns the `alias://` asset source, for the same reason it owns `DefaultPlugins`: the
/// source has to be registered before `AssetPlugin` builds, and only the group holding
/// `AssetPlugin` can guarantee that. `with_content_search_paths` is how a target says which
/// directories to scan.
pub struct EnginePlugins {
    window: Window,
    log_capacity: usize,
    content_search_paths: Vec<String>,
    load_order_file: Option<PathBuf>,
}

impl Default for EnginePlugins {
    fn default() -> Self {
        Self {
            window: Window::default(),
            log_capacity: log::DEFAULT_CAPACITY,
            content_search_paths: Vec::new(),
            load_order_file: None,
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

    /// Sets the directories scanned for content packages, relative to the asset root. Empty by
    /// default: which directories a game ships is game policy, not engine policy.
    pub fn with_content_search_paths<I, S>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.content_search_paths = paths.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the user's load order file, the one place a player rather than an author orders
    /// packages. `None` by default: where a config file lives is the binary's decision.
    pub fn with_load_order_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.load_order_file = Some(path.into());
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
            // Before `AssetPlugin`, not merely early: `register_asset_source` fills a resource
            // that `AssetPlugin` turns into live sources exactly once, when it builds. Registering
            // afterwards logs an error and leaves `alias://` dead. `add_before` panics if
            // `AssetPlugin` is not in the group, so a future `.disable::<AssetPlugin>()` fails
            // loudly here instead of silently at the first load.
            .add_before::<AssetPlugin>({
                let mut plugin =
                    AssetContentPlugin::default().with_search_paths(self.content_search_paths);
                if let Some(path) = self.load_order_file {
                    plugin = plugin.with_load_order_file(path);
                }
                plugin
            })
            .disable::<TransformPlugin>()
            .add_group(BigSpaceDefaultPlugins)
            // No longer bundled with BigSpaceDefaultPlugins as of big_space 0.13.
            .add_group(BigSpaceDebugPlugins::default())
            .add(bigspace::BigSpacePlugin)
            .add(debug_overlay::DebugOverlayPlugin)
            .add(camera::VirtualCameraPlugin)
            .add(stage::StagePlugin)
            .add(physics::PhysicsIntegrationPlugin)
            .add(time::TimeControlPlugin)
            // After `DefaultPlugins`, because it resizes the buffer `LogPlugin` just built.
            .add(log::LogBufferPlugin {
                capacity: self.log_capacity,
            })
    }
}
