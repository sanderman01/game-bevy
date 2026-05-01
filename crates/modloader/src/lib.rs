mod manifest;

use bevy::{
    app::{Plugin, Startup, Update},
    asset::{AssetApp, AssetPath, AssetServer, Handle, io::AssetSourceId},
    ecs::{
        resource::Resource,
        system::{Commands, ResMut},
    },
    log::info,
    tasks::{BoxedFuture, IoTaskPool, Task, futures::check_ready},
};

pub use crate::manifest::*;

const DEFAULT_PACKAGE_SEARCH_PATHS: &[&str] = &["basegame", "mods"];

pub struct ModLoaderPlugin {
    package_search_paths: Vec<String>,
}

impl Default for ModLoaderPlugin {
    fn default() -> Self {
        Self {
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
            .insert_resource(Manifests::default())
            .insert_resource(PackageLoader {
                package_search_paths: self.package_search_paths.to_owned(),
                scan_task: None,
            })
            .add_systems(Startup, discover_packages)
            .add_systems(Update, poll_manifests_task);
    }
}

#[derive(Resource)]
pub struct PackageLoader {
    package_search_paths: Vec<String>,
    scan_task: Option<Task<Vec<Handle<Manifest>>>>,
}

pub fn discover_packages(server: ResMut<AssetServer>, mut loader: ResMut<PackageLoader>) {
    let paths: Vec<AssetPath> = loader
        .package_search_paths
        .iter()
        .map(|p| AssetPath::from(p.to_owned()).with_source(AssetSourceId::Default))
        .collect();
    info!("Scanning for packages at paths: '{:?}'", paths);
    let paths = paths.clone();

    let futures: Vec<BoxedFuture<Vec<Handle<Manifest>>>> = paths
        .iter()
        .map(|p| scan_for_package_manifests(p.to_owned(), server.clone()))
        .collect();
    let task = IoTaskPool::get().spawn(async move {
        let mut handles = Vec::<Handle<Manifest>>::new();
        for fut in futures {
            handles.append(&mut fut.await);
        }
        handles
    });

    loader.scan_task = Some(task);
}

fn poll_manifests_task(
    mut commands: Commands,
    server: ResMut<AssetServer>,
    mut loader: ResMut<PackageLoader>,
) {
    // Check if the background thread is done AND the manifests are loaded.
    if let Some(task) = &mut loader.scan_task {
        if let Some(manifests) = check_ready(task)
            && manifests.iter().all(|m| server.is_loaded(m))
        {
            // We are done loading manifests.
            info!("Loaded {} manifests:", manifests.len());
            for m in &manifests {
                info!("  {}", m.path().unwrap());
            }

            commands.insert_resource(Manifests {
                items: manifests.clone(),
            });
            loader.scan_task = None;
        }
    }
}
