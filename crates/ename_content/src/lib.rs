//! `ename_content` -- packages, manifests, and the asset alias registry.
//!
//! Being replaced by `ename_asset_alias`, `ename_asset_package` and `ename_asset_content`, and
//! deleted once `ename_game` addresses assets through `alias://`. Nothing here is worth extending.

mod asset_registry;
mod package;

use bevy::{
    app::Plugin,
    ecs::{resource::Resource, system::ResMut},
    log::info,
    state::{
        app::AppExtStates,
        state::{NextState, OnEnter, States},
    },
};

pub use crate::asset_registry::AssetRegistry;
pub use crate::package::{Package, PackageState, Packages};
// The manifest types moved to `ename_asset_package`. This crate is deleted in Task 7; until then
// it re-exports them so `ename_game` keeps compiling against the old names.
pub use ename_asset_package::{AssetsInfo, Manifest, PackageInfo, Version, VersionError};

/// No search paths by default. Which directories a game ships is game policy: the binary
/// passes them in with [`ContentPlugin::with_search_paths`].
const DEFAULT_PACKAGE_SEARCH_PATHS: &[&str] = &[];
const DEFAULT_PACKAGE_STATE: PackageState = PackageState::Active;

/// Registers the [`AssetRegistry`], the package lists, and the [`LoaderState`] machine.
///
/// The scan itself is gone: `ename_asset_content` owns it now. What is left runs the state machine
/// to its end so `ename_game`'s gate still opens, and registers nothing.
pub struct ContentPlugin {
    package_search_paths: Vec<String>,
    default_package_active_state: PackageState,
}

impl Default for ContentPlugin {
    fn default() -> Self {
        Self {
            default_package_active_state: DEFAULT_PACKAGE_STATE,
            package_search_paths: DEFAULT_PACKAGE_SEARCH_PATHS
                .iter()
                .map(|s| String::from(*s))
                .collect(),
        }
    }
}

impl ContentPlugin {
    /// Sets the directories scanned for package manifests, relative to the asset root.
    pub fn with_search_paths<I, S>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.package_search_paths = paths.into_iter().map(Into::into).collect();
        self
    }
}

impl Plugin for ContentPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        app.init_resource::<AssetRegistry>()
            .insert_resource(Packages::default())
            .insert_resource(PackageLoader {
                default_package_state: self.default_package_active_state,
                package_search_paths: self.package_search_paths.to_owned(),
            })
            .init_state::<LoaderState>()
            .add_systems(OnEnter(LoaderState::Startup), on_init)
            .add_systems(OnEnter(LoaderState::PackagesRegistering), register_packages)
            .add_systems(
                OnEnter(LoaderState::PackagesRegistered),
                on_packages_registered,
            )
            .add_systems(
                OnEnter(LoaderState::AssetsRegistering),
                register_packages_assets,
            );
    }
}

#[derive(Resource)]
pub(crate) struct PackageLoader {
    #[expect(
        dead_code,
        reason = "the scan that read this lives in ename_asset_content now"
    )]
    package_search_paths: Vec<String>,
    #[expect(dead_code, reason = "nothing constructs a Package here any more")]
    default_package_state: PackageState,
}

/// Keeps track of where we are in the process of loading packages
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Hash, States)]
pub enum LoaderState {
    #[default]
    Startup,
    ManifestsScanning,
    ManifestsLoading,
    PackagesRegistering,
    PackagesRegistered,
    AssetsRegistering,
    AssetsRegistered,
}

fn on_init(mut state: ResMut<NextState<LoaderState>>) {
    info!("Content init");
    state.set(LoaderState::PackagesRegistering);
}

fn register_packages(packages: ResMut<Packages>, mut state: ResMut<NextState<LoaderState>>) {
    packages.into_inner().items = Vec::new();
    state.set(LoaderState::PackagesRegistered);
}

fn on_packages_registered(mut state: ResMut<NextState<LoaderState>>) {
    state.set(LoaderState::AssetsRegistering);
}

fn register_packages_assets(mut state: ResMut<NextState<LoaderState>>) {
    state.set(LoaderState::AssetsRegistered);
}
