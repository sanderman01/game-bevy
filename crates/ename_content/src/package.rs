use crate::Manifest;
use bevy::asset::AssetPath;
use bevy::prelude::*;
use bevy::{asset::Handle, ecs::resource::Resource, reflect::Reflect};

/// Resource containing all packages
#[derive(Default, Debug, Clone, Resource, Reflect)]
#[reflect(Default, Debug, Clone, Resource)]
pub struct Packages {
    pub items: Vec<Package>,
}

/// Used to keep track of packages.
#[derive(Debug, Clone, PartialEq, Reflect)]
pub struct Package {
    pub state: PackageState,
    pub manifest: Handle<Manifest>,
}

impl Package {
    pub fn path(&self) -> Option<&AssetPath<'static>> {
        self.manifest.path()
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Reflect)]
pub enum PackageState {
    Inactive,
    Active,
}
