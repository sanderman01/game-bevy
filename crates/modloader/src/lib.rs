mod assetregistry;
mod manifest;
mod package;

use bevy::{
    app::{Plugin, Update},
    asset::{AssetApp, AssetPath, AssetServer, Assets, Handle, LoadState, io::AssetSourceId},
    ecs::{
        resource::Resource,
        schedule::IntoScheduleConfigs,
        system::{Res, ResMut},
    },
    log::info,
    state::{
        app::AppExtStates,
        condition::in_state,
        state::{NextState, OnEnter, States},
    },
    tasks::{BoxedFuture, IoTaskPool, Task, futures::check_ready},
};

pub use crate::assetregistry::*;
pub use crate::manifest::*;
pub use crate::package::*;

const NAMESPACE_DELIM_TOKEN: &str = "::";

const DEFAULT_AUTO_LOAD_PACKAGE_MANIFESTS: bool = true;
const DEFAULT_AUTO_LOAD_PACKAGE_ASSETS: bool = true;
const DEFAULT_PACKAGE_SEARCH_PATHS: &[&str] = &["basegame", "mods"];
const DEFAULT_PACKAGE_STATE: PackageState = PackageState::Active;

pub struct ModLoaderPlugin {
    package_search_paths: Vec<String>,
    auto_load_package_manifests: bool,
    auto_load_package_assets: bool,
    default_package_active_state: PackageState,
}

impl Default for ModLoaderPlugin {
    fn default() -> Self {
        Self {
            auto_load_package_manifests: DEFAULT_AUTO_LOAD_PACKAGE_MANIFESTS,
            auto_load_package_assets: DEFAULT_AUTO_LOAD_PACKAGE_ASSETS,
            default_package_active_state: DEFAULT_PACKAGE_STATE,
            package_search_paths: DEFAULT_PACKAGE_SEARCH_PATHS
                .iter()
                .map(|s| String::from(*s))
                .collect(),
        }
    }
}

impl Plugin for ModLoaderPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        app.init_asset::<Manifest>()
            .register_type::<Manifest>()
            .register_type::<Manifests>()
            .register_asset_reflect::<Manifest>()
            .init_asset_loader::<ManifestAssetLoader>()
            .insert_resource(AssetRegistry::new())
            .insert_resource(Manifests::default())
            .insert_resource(Packages::default())
            .insert_resource(PackageLoader {
                default_package_state: self.default_package_active_state,
                package_search_paths: self.package_search_paths.to_owned(),
                auto_load_package_manifests: self.auto_load_package_manifests,
                auto_load_package_assets: self.auto_load_package_assets,
                scan_task: None,
            })
            .init_state::<LoaderState>()
            .add_systems(OnEnter(LoaderState::Startup), on_init)
            .add_systems(
                OnEnter(LoaderState::ManifestsScanning),
                scan_package_manifests,
            )
            .add_systems(OnEnter(LoaderState::PackagesRegistering), register_packages)
            .add_systems(
                OnEnter(LoaderState::PackagesRegistered),
                on_packages_registered,
            )
            .add_systems(
                OnEnter(LoaderState::AssetsRegistering),
                register_packages_assets,
            )
            .add_systems(
                Update,
                poll_manifests_scan_task.run_if(in_state(LoaderState::ManifestsScanning)),
            )
            .add_systems(
                Update,
                poll_manifests_loaded.run_if(in_state(LoaderState::ManifestsLoading)),
            );
    }
}

#[derive(Resource)]
pub struct PackageLoader {
    package_search_paths: Vec<String>,
    auto_load_package_manifests: bool,
    auto_load_package_assets: bool,
    default_package_state: PackageState,
    scan_task: Option<Task<Vec<Handle<Manifest>>>>,
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
}

pub fn on_init(loader: Res<PackageLoader>, mut state: ResMut<NextState<LoaderState>>) {
    info!("Modloader Init");
    if loader.auto_load_package_manifests {
        state.set(LoaderState::ManifestsScanning);
    }
}

pub fn scan_package_manifests(server: ResMut<AssetServer>, mut loader: ResMut<PackageLoader>) {
    let paths: Vec<AssetPath> = loader
        .package_search_paths
        .iter()
        .map(|p| AssetPath::from(p.to_owned()).with_source(AssetSourceId::Default))
        .collect();
    info!("Scanning for packages at paths: {:?}", paths);
    let paths = paths.clone();

    let futures: Vec<BoxedFuture<Vec<Handle<Manifest>>>> = paths
        .iter()
        .map(|p| scan_for_package_manifests(p.to_owned(), server.clone()))
        .collect();
    let task = IoTaskPool::get().spawn(async move {
        let mut manifests = Vec::<Handle<Manifest>>::new();
        for fut in futures {
            manifests.append(&mut fut.await);
        }
        manifests
    });

    loader.scan_task = Some(task);
}

fn poll_manifests_scan_task(
    mut res_manifests: ResMut<Manifests>,
    mut loader: ResMut<PackageLoader>,
    mut state: ResMut<NextState<LoaderState>>,
) {
    // Check if the scanning task is done.
    if let Some(task) = &mut loader.scan_task
        && let Some(manifests) = check_ready(task)
    {
        res_manifests.items = manifests;
        loader.scan_task = None;
        state.set(LoaderState::ManifestsLoading);
        info!("Scan Task Done.");
    }
}

fn poll_manifests_loaded(
    mut res_manifests: ResMut<Manifests>,
    mut state: ResMut<NextState<LoaderState>>,
    server: ResMut<AssetServer>,
) {
    if !res_manifests.items.iter().all(|h| {
        let load_state = server.load_state(h);
        matches!(load_state, LoadState::Loaded) || matches!(load_state, LoadState::Failed(_))
    }) {
        return;
    }

    // We are done loading manifests.
    // Note. Some of them may have failed to load. (eg. due to parsing failure)
    // To avoid panics later, keep only those that loaded successfully.
    res_manifests
        .items
        .retain(|h| matches!(server.load_state(h), LoadState::Loaded));

    state.set(LoaderState::PackagesRegistering);
    info!("Loaded {} manifests:", res_manifests.items.len());
    for m in &res_manifests.items {
        info!("  {}", m.path().unwrap());
    }
}

pub fn register_packages(
    loader: Res<PackageLoader>,
    manifests: Res<Manifests>,
    packages: ResMut<Packages>,
    manifest_assets: Res<Assets<Manifest>>,
    mut state: ResMut<NextState<LoaderState>>,
    server: Res<AssetServer>,
) {
    // TODO Some conventions plus user settings/preferences data indicating which packages should be active/inactive.

    let mut pkgs: Vec<Package> = manifests
        .items
        .iter()
        .map(|m| Package {
            manifest: m.clone(),
            state: loader.default_package_state,
        })
        .collect();

    // The earlier poll_manifests_loaded should have already filtered out
    // non-loaded manifests. But that may not apply if the state-machine is bypassed.
    pkgs.retain(|p| server.is_loaded(&p.manifest));
    pkgs.sort_by(|a, b| {
        let a = manifest_assets.get(&a.manifest).unwrap();
        let b = manifest_assets.get(&b.manifest).unwrap();
        a.package.load_priority.cmp(&b.package.load_priority)
    });

    let packages = packages.into_inner();
    packages.items = pkgs;

    state.set(LoaderState::PackagesRegistered);
}

pub fn on_packages_registered(
    loader: Res<PackageLoader>,
    mut state: ResMut<NextState<LoaderState>>,
) {
    if loader.auto_load_package_assets {
        state.set(LoaderState::AssetsRegistering);
    }
}

pub fn register_packages_assets(
    packages: Res<Packages>,
    manifests_assets: Res<Assets<Manifest>>,
    registry: Res<AssetRegistry>,
) {
    let registry = registry.clone();

    let packages = &packages.items;
    info!("Register package assets:");

    for pkg in packages
        .iter()
        .filter(|pkg| matches!(pkg.state, PackageState::Active))
    {
        if let Some(manifest) = manifests_assets.get(&pkg.manifest) {
            let pkg_id = manifest.package.id.clone();
            let manifest_path = match pkg.path() {
                Some(val) => val,
                None => continue,
            };

            info!("  {:20}    {:?}", pkg_id, manifest_path);

            let pkg_root_path = manifest_path.path().parent().unwrap().to_string_lossy();
            register_package_assets(&registry, manifest, &pkg_root_path);
        }
    }
}

pub fn register_package_assets(registry: &AssetRegistry, manifest: &Manifest, pkg_root_path: &str) {
    let pkg_id = &manifest.package.id;

    if let Some(add) = &manifest.assets.add {
        for kvp in add.iter() {
            let alias = kvp.0;
            let path = kvp.1;
            let path = format!("{}/{}", pkg_root_path, path);
            let namespaced_alias = format!("{}{}{}", pkg_id, NAMESPACE_DELIM_TOKEN, alias);
            registry.register_asset(&namespaced_alias, &path);
            info!("  ++ {:20} {}", &namespaced_alias, &path);
        }
    }

    if let Some(replace) = &manifest.assets.replace {
        for kvp in replace.iter() {
            let alias = kvp.0;
            let path = kvp.1;
            let path = format!("{}/{}", pkg_root_path, path);
            registry.register_asset(&alias, &path);
            info!("  := {:20} {}", &alias, &path);
        }
    }

    if let Some(remove) = &manifest.assets.remove {
        for kvp in remove.iter() {
            let alias = kvp.0;
            registry.unregister_asset(&alias);
            info!("  -- {:20}", &alias);
        }
    }
}
