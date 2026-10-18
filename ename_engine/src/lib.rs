//! `ename_engine` -- the runtime layer: camera rig, big_space integration, physics glue, input,
//! time control, log capture, the command registry, waiting for the world to settle, screenshots,
//! video capture.
//!
//! Sits directly above the asset crates. It must never depend on `ename_editor` or `example_game_lib`:
//! if engine code needs something from a higher layer, move the shared type down or invert the
//! call into an event. It does depend on `ename_asset_content`, because the `alias://` source has
//! to be registered before `AssetPlugin` builds and this group owns `AssetPlugin`.
//! See `docs/design/crate-layout.md`.

pub mod application;
pub mod bigspace;
pub mod camera;
pub mod capture;
pub mod command;
pub mod debug_overlay;
pub mod debug_switch;
pub mod diagnostics;
pub mod log;
pub mod perf;
pub mod physics;
mod readback;
pub mod screenshot;
pub mod stage;
pub mod time;
pub mod video;
pub mod wait;

use application::{ApplicationPaths, ApplicationPlugin, OrganisationName, Paths, ProjectName};
use bevy::{
    app::PluginGroupBuilder, asset::AssetPlugin, log::LogPlugin, prelude::*,
    transform::TransformPlugin,
};
use big_space::plugin::{BigSpaceDebugPlugins, BigSpaceDefaultPlugins};
use ename_asset_content::AssetContentPlugin;
use std::path::PathBuf;

pub use ename_asset_content::DEFAULT_ASSET_ROOT;
/// Re-exported so a target can name the user's load order file without depending on the asset
/// crates itself. It defaults to the persistent data directory; `with_load_order_file` moves it.
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
    organisation_name: OrganisationName,
    project_name: ProjectName,
    paths_override: Option<Paths>,
}

impl EnginePlugins {
    /// The names place the per-user directories ([`application::ApplicationPaths`]), so every
    /// target has to give them; there is no sensible default.
    pub fn new(organisation_name: OrganisationName, project_name: ProjectName) -> Self {
        Self {
            window: Window::default(),
            log_capacity: log::DEFAULT_CAPACITY,
            content_search_paths: Vec::new(),
            load_order_file: None,
            organisation_name,
            project_name,
            paths_override: None,
        }
    }

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
    /// packages. Defaults to [`LOAD_ORDER_FILE`] in the persistent data directory; without one,
    /// there is no load order file.
    pub fn with_load_order_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.load_order_file = Some(path.into());
        self
    }

    /// Uses these directories instead of resolving them from the names, for tests and throwaway
    /// sessions.
    pub fn with_application_paths(mut self, paths: Paths) -> Self {
        self.paths_override = Some(paths);
        self
    }

    /// Resolved from the names, unless `with_application_paths` gave them.
    fn application_paths(&self) -> ApplicationPaths {
        ApplicationPaths::new(
            self.organisation_name.clone(),
            self.project_name.clone(),
            self.paths_override.clone(),
        )
    }

    /// `with_load_order_file` wins; otherwise the file lives in the persistent data directory.
    /// `None` when neither exists.
    fn load_order_file(&self, paths: &ApplicationPaths) -> Option<PathBuf> {
        self.load_order_file.clone().or_else(|| {
            paths
                .persistent_data_path()
                .map(|dir| dir.join(LOAD_ORDER_FILE))
        })
    }
}

impl PluginGroup for EnginePlugins {
    fn build(self) -> PluginGroupBuilder {
        let application_paths = self.application_paths();
        let load_order_file = self.load_order_file(&application_paths);
        let group = DefaultPlugins
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
                if let Some(path) = load_order_file {
                    plugin = plugin.with_load_order_file(path);
                }
                plugin
            })
            // After `LogPlugin`, which installs the logger its lines go to.
            .add_after::<LogPlugin>(ApplicationPlugin {
                paths: application_paths,
            })
            .disable::<TransformPlugin>()
            .add_group(BigSpaceDefaultPlugins)
            // No longer bundled with BigSpaceDefaultPlugins as of big_space 0.13.
            .add_group(BigSpaceDebugPlugins::default())
            .add(bigspace::BigSpacePlugin)
            .add(debug_switch::DebugSwitchPlugin)
            .add(diagnostics::DiagnosticsLogPlugin)
            .add(debug_overlay::DebugOverlayPlugin)
            .add(camera::VirtualCameraPlugin)
            .add(camera::ground::CameraGroundPlugin)
            .add(stage::StagePlugin)
            .add(physics::PhysicsIntegrationPlugin)
            .add(time::TimeControlPlugin)
            .add(command::CommandPlugin)
            .add(wait::WaitPlugin)
            .add(capture::CapturePlugin)
            .add(screenshot::ScreenshotPlugin)
            .add(video::VideoPlugin)
            .add(perf::PerfPlugin);
        // After `DefaultPlugins`, because it resizes the buffer `LogPlugin` just built.
        group.add(log::LogBufferPlugin {
            capacity: self.log_capacity,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORG: OrganisationName = OrganisationName::from_static("ename");
    const PROJECT: ProjectName = ProjectName::from_static("engine-plugins-test");

    fn engine_in(root: &std::path::Path) -> EnginePlugins {
        EnginePlugins::new(ORG, PROJECT)
            .with_application_paths(Paths::from_paths(root.join("data"), root.join("cache")))
    }

    #[test]
    fn builders_reach_the_application_paths() {
        let root = std::env::temp_dir();
        let paths = engine_in(&root).application_paths();
        assert_eq!(paths.organisation(), &ORG);
        assert_eq!(paths.project(), &PROJECT);
        assert_eq!(
            paths.persistent_data_path(),
            Some(root.join("data").as_path())
        );
        assert_eq!(paths.cache_path(), Some(root.join("cache").as_path()));
    }

    #[test]
    fn load_order_file_defaults_to_the_persistent_data_directory() {
        let root = std::env::temp_dir();
        let engine = engine_in(&root);
        assert_eq!(
            engine.load_order_file(&engine.application_paths()),
            Some(root.join("data").join(LOAD_ORDER_FILE))
        );
    }

    #[test]
    fn explicit_load_order_file_wins() {
        let root = std::env::temp_dir();
        let explicit = root.join("elsewhere.toml");
        let engine = engine_in(&root).with_load_order_file(explicit.clone());
        assert_eq!(
            engine.load_order_file(&engine.application_paths()),
            Some(explicit)
        );
    }
}
