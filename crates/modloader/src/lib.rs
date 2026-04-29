mod manifest;

use bevy::{
    app::{Plugin, Startup},
    asset::{AssetApp, AssetServer},
    ecs::system::{Commands, ResMut},
};

pub use crate::manifest::*;

pub struct ModLoaderPlugin;

impl Plugin for ModLoaderPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        app.init_asset::<Manifest>()
            .init_asset_loader::<ManifestAssetLoader>()
            .register_type::<Manifest>()
            .register_type::<Manifests>()
            .register_asset_reflect::<Manifest>()
            .add_systems(Startup, test_load_manifest);
    }
}

/// TODO Remove this function after we done experimenting and testing
fn test_load_manifest(mut commands: Commands, asset_server: ResMut<AssetServer>) {
    let loaded_manifest_handle = asset_server.load::<Manifest>("manifest.toml");
    let mut manifests_resource = Manifests::default();
    manifests_resource.items.push(loaded_manifest_handle);
    commands.insert_resource(manifests_resource);
}
